//! Cross-file symbol resolution.
//!
//! The dependency graph in [`crate::graph::builder`] is keyed on
//! [`FullyQualifiedName`]s, but the raw material collected by the AST extractors
//! is a list of import specifiers (`"./user"`, `os.path`, `crate::ast`) and bare
//! call names. This module bridges the two: it builds a [`SymbolTable`] mapping
//! every exported symbol to a stable [`SymbolId`], and a [`Resolver`] that turns
//! raw module specifiers into real repository file paths.
//!
//! Resolution is best-effort. Anything that cannot be matched to a file in the
//! repository — external packages (`react`, `fmt`, `os`), URLs, absolute paths —
//! is dropped silently. The graph builder relies on this contract: a `None`
//! return means "no edge here, move on".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::{FileNode, Language, Symbol};

/// Opaque identifier for a single exported symbol within a [`SymbolTable`].
///
/// Stable for the lifetime of the table that minted it. The inner `u32` is the
/// table's dense vector index, so lookups by id are O(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(u32);

impl SymbolId {
    /// Returns the raw index used for storage. Intended for tests and
    /// diagnostics only — callers should treat the value as opaque.
    #[must_use]
    pub const fn raw(self) -> usize {
        self.0 as usize
    }

    /// Constructs a `SymbolId` from a raw slot index. Intended for internal
    /// use within the graph module (e.g. translating back from a slot we
    /// received from `top_n_indices`).
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub const fn from_raw(slot: usize) -> Self {
        Self(slot as u32)
    }
}

/// A symbol's address within the repository: the file it lives in plus its
/// declared name. Sufficient to disambiguate within a single repo without
/// reconstructing language-specific scoping rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FullyQualifiedName {
    /// Repository-relative path of the declaring file.
    pub file: PathBuf,
    /// Bare symbol name as written in source.
    pub name: String,
}

impl FullyQualifiedName {
    #[must_use]
    pub fn new(file: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            name: name.into(),
        }
    }
}

/// Read-only map from [`FullyQualifiedName`] to [`SymbolId`] with reverse lookup
/// and a bare-name index for resolving call sites and inheritance references.
///
/// Duplicate names within the same file are allowed: each declaration gets its
/// own `SymbolId`. This is necessary because Rust allows multiple `impl` blocks
/// for the same type (each with methods), and Python allows a function and a
/// class to share a name.
#[allow(clippy::struct_field_names)]
pub struct SymbolTable {
    /// `SymbolId(u32)` -> FQN. Indexed densely so id lookups are O(1).
    by_id: Vec<FullyQualifiedName>,
    /// FQN -> all `SymbolId`s with that FQN. A Vec because the same file can
    /// declare multiple symbols with the same name (e.g. Rust impl methods).
    by_fqn: HashMap<FullyQualifiedName, Vec<SymbolId>>,
    /// Bare symbol name -> all `SymbolId`s with that name. Used when only the
    /// callee name is known (e.g. `foo()` without a qualifier).
    by_name: HashMap<String, Vec<SymbolId>>,
}

impl SymbolTable {
    /// Indexes every *exported* symbol across the given files. Non-exported
    /// symbols are skipped because they cannot be referenced from another file.
    #[must_use]
    pub fn from_files(nodes: &[FileNode]) -> Self {
        let mut table = Self {
            by_id: Vec::new(),
            by_fqn: HashMap::new(),
            by_name: HashMap::new(),
        };

        for node in nodes {
            for symbol in &node.symbols {
                if !symbol.visibility().is_exported() {
                    continue;
                }
                table.insert(node.path.clone(), symbol);
            }
        }

        table
    }

    fn insert(&mut self, file: PathBuf, symbol: &Symbol) {
        let fqn = FullyQualifiedName::new(file, symbol.name());
        #[allow(clippy::cast_possible_truncation)]
        let id = SymbolId(self.by_id.len() as u32);
        self.by_id.push(fqn.clone());
        self.by_fqn.entry(fqn).or_default().push(id);
        self.by_name
            .entry(symbol.name().to_owned())
            .or_default()
            .push(id);
    }

    /// Looks up every symbol id for a fully-qualified name.
    ///
    /// Returns an empty slice if no symbol with that FQN exists. Multiple ids
    /// are returned when the same file declares multiple symbols with the same
    /// name (e.g. methods in different `impl` blocks).
    #[must_use]
    pub fn lookup(&self, fqn: &FullyQualifiedName) -> &[SymbolId] {
        self.by_fqn.get(fqn).map_or(&[], Vec::as_slice)
    }

    /// Looks up every exported symbol with the given bare name, across all files.
    /// Multiple matches are returned in insertion order.
    #[must_use]
    pub fn lookup_by_name(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// Returns the fully-qualified name for the given id.
    #[must_use]
    pub fn fqn(&self, id: SymbolId) -> Option<&FullyQualifiedName> {
        self.by_id.get(id.raw())
    }

    /// Iterates over every `(`[`SymbolId`], &[`FullyQualifiedName`]`)` pair in id order.
    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &FullyQualifiedName)> {
        self.by_id
            .iter()
            .enumerate()
            .map(|(i, fqn)| {
                #[allow(clippy::cast_possible_truncation)]
                let id = SymbolId(i as u32);
                (id, fqn)
            })
    }

    /// Number of indexed symbols.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Resolves raw module specifiers (`./user`, `os.path`, `crate::ast`) to actual
/// repository file paths.
///
/// Constructed from the set of file paths discovered by the walker. The
/// resolution rules are language-specific and intentionally conservative:
/// anything that cannot be matched to a file in the repository returns `None`
/// and is silently dropped by the graph builder.
pub struct Resolver {
    /// Known file paths, normalised to forward slashes for cross-platform matching.
    files: HashSet<String>,
}

impl Resolver {
    /// Builds a resolver from the set of files in the repository.
    /// Paths are normalised to forward-slash form.
    #[must_use]
    pub fn new(files: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        let mut set = HashSet::new();
        for f in files {
            let normalised = normalise_path(f.as_ref());
            set.insert(normalised);
        }
        Self { files: set }
    }

    /// Resolves a module specifier to a real file path in the repository.
    ///
    /// Returns `None` for external packages, unrecognised URLs, and absolute
    /// paths (which are deliberately ignored for security). The returned path
    /// is in forward-slash form regardless of platform.
    #[must_use]
    pub fn resolve(&self, importer: &Path, module: &str, language: &Language) -> Option<PathBuf> {
        if module.is_empty() {
            return None;
        }

        let candidates: Vec<PathBuf> = match language {
            Language::TypeScript | Language::JavaScript => resolve_js(importer, module),
            Language::Python => resolve_python(importer, module),
            Language::Rust => resolve_rust(importer, module),
            Language::Go => resolve_go(importer, module),
            _ => Vec::new(),
        };

        for candidate in candidates {
            let normalised = normalise_path(&candidate);
            if self.files.contains(&normalised) {
                return Some(PathBuf::from(normalised));
            }
        }
        None
    }

}

/// Converts a path to forward-slash form for stable cross-platform comparison.
fn normalise_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR != '/' {
        s = s.replace(std::path::MAIN_SEPARATOR, "/");
    }
    s
}

/// File extensions searched when resolving a JS/TS import, in priority order.
const JS_EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Resolves a JavaScript/TypeScript module specifier to a list of candidate
/// paths. The caller (`Resolver::resolve`) checks each against the file set.
///
/// - `./foo` and `../foo` are resolved relative to the importing file's
///   directory, trying each extension in [`JS_EXTENSIONS`] and an `index.*`
///   fallback.
/// - Bare specifiers (`react`, `lodash`) and absolute paths are treated as
///   external and produce no candidates.
fn resolve_js(importer: &Path, module: &str) -> Vec<PathBuf> {
    if !module.starts_with("./") && !module.starts_with("../") {
        return Vec::new();
    }

    let base_dir = importer.parent().unwrap_or(Path::new("."));
    let joined = join_lexical(base_dir, module);

    let mut out = Vec::new();
    for ext in JS_EXTENSIONS {
        out.push(joined.with_extension(ext));
    }
    for ext in JS_EXTENSIONS {
        out.push(joined.join(format!("index.{ext}")));
    }
    out
}

/// Joins a relative module specifier to a base path, normalising `.` and `..`
/// components lexically without touching the filesystem.
///
/// `PathBuf::join("./foo")` leaves a `.` component in the path, which then
/// fails to match against the file set (the set was built from canonicalised
/// paths during the walk). This helper produces a path with no `.` or `..`
/// components, so string comparison against the file set works.
fn join_lexical(base: &Path, module: &str) -> PathBuf {
    let mut result = base.to_path_buf();
    for component in module.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// Resolves a Python module specifier to a list of candidate paths.
///
/// - `import a.b.c` is resolved from the repository root: looks for `a/b/c.py`
///   or `a/b/c/__init__.py`. Since the repo root is unknown, we probe from each
///   ancestor of the importer.
/// - `from .sibling import x` is resolved relative to the importing file's
///   directory: `sibling.py` or `sibling/__init__.py`.
/// - `from ..pkg import x` walks up one level, then resolves `pkg`.
/// - Leading-dots specifiers with no name (`from . import x`) resolve to the
///   importing file's own package (`__init__.py` of its directory).
fn resolve_python(importer: &Path, module: &str) -> Vec<PathBuf> {
    // Pure dots means "current package".
    if module.chars().all(|c| c == '.') {
        let dir = importer.parent().unwrap_or(Path::new("."));
        return vec![dir.join("__init__.py")];
    }

    if module.starts_with('.') {
        // Relative: count leading dots to determine how many levels to walk up.
        let leading_dots = module
            .chars()
            .take_while(|c| *c == '.')
            .count();
        let rest = &module[leading_dots..];

        let mut base = importer.parent().unwrap_or(Path::new(".")).to_path_buf();
        // First dot is "current package", each additional dot is one level up.
        for _ in 1..leading_dots {
            if !base.pop() {
                return Vec::new();
            }
        }
        return python_candidates(&base, rest);
    }

    // Absolute dotted name -> walk up importer's ancestors probing each.
    let dotted_path = module.replace('.', "/");
    let mut base = importer.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut out = Vec::new();
    loop {
        out.extend(python_candidates(&base, &dotted_path));
        if !base.pop() {
            break;
        }
    }
    out
}

/// Builds the two candidate paths for a Python module: `<base>/<rel>.py`
/// and `<base>/<rel>/__init__.py`.
fn python_candidates(base: &Path, rel: &str) -> Vec<PathBuf> {
    if rel.is_empty() {
        return vec![base.join("__init__.py")];
    }
    vec![
        base.join(format!("{rel}.py")),
        base.join(rel).join("__init__.py"),
    ]
}

/// Resolves a Rust `use` path to a list of candidate file paths.
///
/// - `crate::foo::bar` is resolved from the crate root. We don't know which
///   file is the crate root, so we walk up the importer's ancestors looking for
///   `foo/bar.rs` or `foo/bar/mod.rs` at each level.
/// - `super::foo` walks one level up from the importer's directory, then
///   resolves `foo.rs` or `foo/mod.rs` there.
/// - `self::foo` resolves in the importer's own directory.
/// - `::std::...` and any other path containing `::` that isn't prefixed with
///   `crate::`, `super::`, or `self::` is an extern-crate reference → no
///   candidates.
/// - A single bare identifier (no `::`) is treated as a same-module reference.
fn resolve_rust(importer: &Path, module: &str) -> Vec<PathBuf> {
    if module.is_empty() {
        return Vec::new();
    }

    // Determine the path segments (after stripping any prefix) and the
    // directory to start searching from.
    let (segments, start_dir) = match parse_rust_prefix(importer, module) {
        RustUse::External => return Vec::new(),
        RustUse::Local { segments, start_dir } => (segments, start_dir),
    };

    if segments.is_empty() {
        return Vec::new();
    }

    // Reconstruct the relative path: foo/bar/baz
    let rel_path: PathBuf = segments.iter().collect::<PathBuf>();

    // Walk up from start_dir, trying `<base>/foo/bar.rs` and
    // `<base>/foo/bar/mod.rs` at each ancestor. We don't know which ancestor
    // is the crate root, so we try them all.
    let mut base = start_dir;
    let mut out = Vec::new();
    loop {
        out.push(base.join(&rel_path).with_extension("rs"));
        out.push(base.join(&rel_path).join("mod.rs"));
        if !base.pop() {
            break;
        }
    }
    out
}

/// Classification of a Rust `use` path into its prefix kind.
enum RustUse {
    /// External crate (e.g. `std::collections::HashMap`, `::serde::Deserialize`).
    External,
    /// Local path with segments and a starting directory.
    Local {
        segments: Vec<String>,
        start_dir: PathBuf,
    },
}

/// Parses the prefix of a Rust `use` path and returns the remaining segments
/// plus the directory to start resolution from.
fn parse_rust_prefix(importer: &Path, module: &str) -> RustUse {
    let importer_dir = importer.parent().unwrap_or(Path::new(".")).to_path_buf();

    if let Some(rest) = module.strip_prefix("crate::") {
        // `crate::foo::bar` → segments ["foo", "bar"], start from importer_dir
        // (will walk up to find the crate root).
        let segments: Vec<String> = rest
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        RustUse::Local {
            segments,
            start_dir: importer_dir,
        }
    } else if module == "crate" {
        // Bare `crate` — the crate root itself. No segments to resolve.
        RustUse::Local {
            segments: Vec::new(),
            start_dir: importer_dir,
        }
    } else if module.starts_with("super") {
        // Count `super::` prefixes to determine how many levels to walk up.
        //
        // In Rust, `super::foo` from `src/a/b.rs` means "foo in module `a`",
        // and module `a`'s directory IS `src/a/` (the importer's parent dir).
        // So the first `super` needs ZERO pops — we look in `importer.parent()`.
        // Each subsequent `super` pops one more directory.
        //
        // Examples (importer = src/a/b.rs, importer.parent() = src/a/):
        //   super::foo       → 0 pops → look in src/a/
        //   super::super::foo → 1 pop  → look in src/
        //   super::super::super::foo → 2 pops → look in repo root
        let mut remaining = module;
        let mut super_count: usize = 0;
        loop {
            if let Some(rest) = remaining.strip_prefix("super::") {
                super_count += 1;
                remaining = rest;
            } else if remaining == "super" {
                super_count += 1;
                remaining = "";
                break;
            } else {
                break;
            }
        }

        let pops_needed = super_count.saturating_sub(1);
        let mut base = importer_dir;
        for _ in 0..pops_needed {
            if !base.pop() {
                return RustUse::External;
            }
        }

        let segments: Vec<String> = if remaining.is_empty() {
            Vec::new()
        } else {
            remaining
                .split("::")
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        RustUse::Local {
            segments,
            start_dir: base,
        }
    } else if let Some(rest) = module.strip_prefix("self::") {
        let segments: Vec<String> = rest
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        RustUse::Local {
            segments,
            start_dir: importer_dir,
        }
    } else if module == "self" {
        RustUse::Local {
            segments: Vec::new(),
            start_dir: importer_dir,
        }
    } else if module.starts_with("::") || module.contains("::") {
        // `::std::...` or any other path with `::` that isn't one of the
        // prefixes above is an extern-crate reference.
        RustUse::External
    } else {
        // A single bare identifier — treat it as a same-module reference.
        RustUse::Local {
            segments: vec![module.to_owned()],
            start_dir: importer_dir,
        }
    }
}

/// Resolves a Go import path.
///
/// Go imports are URLs (`github.com/org/pkg/sub`). Without GOPATH awareness we
/// try every suffix of the import path as a potential directory within the
/// repository. For `github.com/org/pkg/sub`, we try:
///
/// - `sub/` (last segment)
/// - `pkg/sub/` (last two segments)
/// - `org/pkg/sub/` (last three segments)
/// - etc.
///
/// Each suffix is probed at every ancestor of the importer's directory, so we
/// find the match regardless of where the repo root is.
///
/// Go imports resolve to **directories** (packages), not individual files.
/// The caller is expected to match the resolved directory against any file in
/// that directory.
fn resolve_go(importer: &Path, module: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = module.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Vec::new();
    }

    // Build all suffixes: for [a, b, c], produce [c, b/c, a/b/c].
    let suffixes: Vec<PathBuf> = (0..segments.len())
        .map(|start| {
            segments[start..]
                .iter()
                .collect::<PathBuf>()
        })
        .collect();

    // For each suffix, probe at every ancestor of the importer's directory.
    let mut base = importer.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut out = Vec::new();
    loop {
        for suffix in &suffixes {
            out.push(base.join(suffix));
        }
        if !base.pop() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileNode, FunctionDef, SourceLocation, Visibility};
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

    fn resolver_for(tmp: &TempDir, files: &[&str]) -> Resolver {
        let abs: Vec<PathBuf> = files.iter().map(|p| tmp.path().join(p)).collect();
        Resolver::new(abs.iter())
    }

    fn make_node(path: &str, symbols: Vec<Symbol>) -> FileNode {
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

    fn pub_fn(name: &str) -> Symbol {
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

    fn priv_fn(name: &str) -> Symbol {
        Symbol::from(FunctionDef {
            name: name.into(),
            visibility: Visibility::Private,
            signature: format!("fn {name}()"),
            docstring: None,
            parameters: vec![],
            return_type: None,
            body_stripped: false,
            loc: SourceLocation::line_only(PathBuf::from("a.rs"), 1),
        })
    }

    #[test]
    fn symbol_table_indexes_exported_only() {
        let node = make_node("a.rs", vec![pub_fn("pub_fn"), priv_fn("priv_fn")]);
        let table = SymbolTable::from_files(&[node]);
        assert_eq!(table.len(), 1);
        assert!(!table.lookup_by_name("pub_fn").is_empty());
        assert!(table.lookup_by_name("priv_fn").is_empty());
    }

    #[test]
    fn symbol_table_lookups_round_trip() {
        let node = make_node("a.rs", vec![pub_fn("foo")]);
        let table = SymbolTable::from_files(&[node]);
        let id = table.lookup_by_name("foo")[0];
        let fqn = table.fqn(id).unwrap();
        assert_eq!(fqn.name, "foo");
        assert_eq!(fqn.file, PathBuf::from("a.rs"));
        let same_ids = table.lookup(&FullyQualifiedName::new("a.rs", "foo"));
        assert_eq!(same_ids.len(), 1);
        assert_eq!(id, same_ids[0]);
    }

    #[test]
    fn symbol_table_allows_duplicate_names_in_same_file() {
        // Rust allows multiple impl blocks for the same type, each with
        // methods. Python allows a function and a class with the same name.
        // The table must index both, not silently drop one.
        let node = make_node(
            "a.rs",
            vec![pub_fn("process"), pub_fn("process")],
        );
        let table = SymbolTable::from_files(&[node]);
        let ids = table.lookup_by_name("process");
        assert_eq!(
            ids.len(),
            2,
            "duplicate names should both be indexed, not dropped"
        );
        let fqn_ids = table.lookup(&FullyQualifiedName::new("a.rs", "process"));
        assert_eq!(fqn_ids.len(), 2);
    }

    #[test]
    fn resolves_typescript_relative_import() {
        let tmp = write_repo(&[
            ("src/users.ts", "export class User {}"),
            ("src/index.ts", "import { User } from './users';"),
        ]);
        let r = resolver_for(&tmp, &["src/users.ts", "src/index.ts"]);
        let importer = tmp.path().join("src/index.ts");
        let resolved = r.resolve(&importer, "./users", &Language::TypeScript);
        let expected = tmp.path().join("src/users.ts");
        assert_eq!(resolved.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn resolves_typescript_index_file() {
        let tmp = write_repo(&[
            ("src/users/index.ts", "export class User {}"),
            ("src/index.ts", "import { User } from './users';"),
        ]);
        let r = resolver_for(&tmp, &["src/users/index.ts", "src/index.ts"]);
        let importer = tmp.path().join("src/index.ts");
        let resolved = r.resolve(&importer, "./users", &Language::TypeScript);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/users/index.ts").as_path())
        );
    }

    #[test]
    fn drops_external_typescript_import() {
        let tmp = write_repo(&[("src/index.ts", "import React from 'react';")]);
        let r = resolver_for(&tmp, &["src/index.ts"]);
        let importer = tmp.path().join("src/index.ts");
        assert!(r.resolve(&importer, "react", &Language::TypeScript).is_none());
    }

    #[test]
    fn resolves_python_dotted_import() {
        let tmp = write_repo(&[
            ("pkg/sub/mod.py", "def foo(): pass"),
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("app.py", "from pkg.sub import mod"),
        ]);
        let r = resolver_for(
            &tmp,
            &["pkg/sub/mod.py", "app.py", "pkg/__init__.py", "pkg/sub/__init__.py"],
        );
        let importer = tmp.path().join("app.py");
        let resolved = r.resolve(&importer, "pkg.sub.mod", &Language::Python);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("pkg/sub/mod.py").as_path())
        );
    }

    #[test]
    fn resolves_python_relative_import() {
        let tmp = write_repo(&[
            ("pkg/sibling.py", "def helper(): pass"),
            ("pkg/mod.py", "from .sibling import helper"),
        ]);
        let r = resolver_for(&tmp, &["pkg/sibling.py", "pkg/mod.py"]);
        let importer = tmp.path().join("pkg/mod.py");
        let resolved = r.resolve(&importer, ".sibling", &Language::Python);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("pkg/sibling.py").as_path())
        );
    }

    #[test]
    fn drops_external_python_import() {
        let tmp = write_repo(&[("app.py", "import os")]);
        let r = resolver_for(&tmp, &["app.py"]);
        let importer = tmp.path().join("app.py");
        assert!(r.resolve(&importer, "os", &Language::Python).is_none());
    }

    #[test]
    fn resolves_rust_crate_path() {
        let tmp = write_repo(&[
            ("src/lib.rs", "mod users;"),
            ("src/users.rs", "pub struct User;"),
        ]);
        let r = resolver_for(&tmp, &["src/lib.rs", "src/users.rs"]);
        let importer = tmp.path().join("src/lib.rs");
        let resolved = r.resolve(&importer, "crate::users", &Language::Rust);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/users.rs").as_path())
        );
    }

    #[test]
    fn resolves_rust_crate_nested_path() {
        // `crate::models::user` from src/lib.rs should resolve to
        // src/models/user.rs (NOT src/user.rs — the old bug).
        let tmp = write_repo(&[
            ("src/lib.rs", "pub mod models;"),
            ("src/models/mod.rs", "pub mod user;"),
            ("src/models/user.rs", "pub struct User;"),
        ]);
        let r = resolver_for(&tmp, &["src/lib.rs", "src/models/mod.rs", "src/models/user.rs"]);
        let importer = tmp.path().join("src/lib.rs");
        let resolved = r.resolve(&importer, "crate::models::user", &Language::Rust);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/models/user.rs").as_path()),
            "crate::models::user should resolve to src/models/user.rs"
        );
    }

    #[test]
    fn resolves_rust_super_path() {
        // `super::utils` from src/auth/login.rs should resolve to
        // src/utils.rs (one level up from src/auth/).
        let tmp = write_repo(&[
            ("src/auth/login.rs", "use super::utils;"),
            ("src/utils.rs", "pub fn helper() {}"),
        ]);
        let r = resolver_for(&tmp, &["src/auth/login.rs", "src/utils.rs"]);
        let importer = tmp.path().join("src/auth/login.rs");
        let resolved = r.resolve(&importer, "super::utils", &Language::Rust);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/utils.rs").as_path()),
            "super::utils from src/auth/login.rs should resolve to src/utils.rs"
        );
    }

    #[test]
    fn resolves_rust_double_super() {
        // `super::super::root` from src/a/b.rs should resolve to src/root.rs.
        let tmp = write_repo(&[
            ("src/a/b.rs", "use super::super::root;"),
            ("src/root.rs", "pub fn root() {}"),
        ]);
        let r = resolver_for(&tmp, &["src/a/b.rs", "src/root.rs"]);
        let importer = tmp.path().join("src/a/b.rs");
        let resolved = r.resolve(&importer, "super::super::root", &Language::Rust);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/root.rs").as_path()),
            "super::super::root from src/a/b.rs should resolve to src/root.rs"
        );
    }

    #[test]
    fn resolves_rust_self_path() {
        // `self::helper` from src/lib.rs should resolve to src/helper.rs.
        let tmp = write_repo(&[
            ("src/lib.rs", "use self::helper;"),
            ("src/helper.rs", "pub fn helper() {}"),
        ]);
        let r = resolver_for(&tmp, &["src/lib.rs", "src/helper.rs"]);
        let importer = tmp.path().join("src/lib.rs");
        let resolved = r.resolve(&importer, "self::helper", &Language::Rust);
        assert_eq!(
            resolved.as_deref(),
            Some(tmp.path().join("src/helper.rs").as_path())
        );
    }

    #[test]
    fn drops_external_rust_use() {
        let tmp = write_repo(&[("src/lib.rs", "use std::collections::HashMap;")]);
        let r = resolver_for(&tmp, &["src/lib.rs"]);
        let importer = tmp.path().join("src/lib.rs");
        assert!(r
            .resolve(&importer, "std::collections::HashMap", &Language::Rust)
            .is_none());
    }

    #[test]
    fn empty_module_returns_none() {
        let r = Resolver::new(std::iter::empty::<PathBuf>());
        assert!(r
            .resolve(Path::new("a.ts"), "", &Language::TypeScript)
            .is_none());
    }
}
