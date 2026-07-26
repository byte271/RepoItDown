//! Core data types: languages, symbols, visibility, source locations, and file nodes.

use std::path::PathBuf;
use std::str::FromStr;

use crate::error::{Error, Result};

/// Programming language detected from file extension or shebang.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Swift,
    Kotlin,
    Other(String),
}

impl Language {
    pub const UNKNOWN: &'static str = "unknown";

    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        let lower = ext.to_ascii_lowercase();
        match lower.as_bytes() {
            b"rs" => Some(Self::Rust),
            b"py" | b"pyi" | b"pyx" => Some(Self::Python),
            b"ts" | b"tsx" => Some(Self::TypeScript),
            b"js" | b"jsx" | b"mjs" | b"cjs" => Some(Self::JavaScript),
            b"go" => Some(Self::Go),
            b"java" => Some(Self::Java),
            b"c" | b"h" => Some(Self::C),
            b"cpp" | b"cc" | b"cxx" | b"hpp" | b"hxx" => Some(Self::Cpp),
            b"cs" => Some(Self::CSharp),
            b"rb" => Some(Self::Ruby),
            b"swift" => Some(Self::Swift),
            b"kt" | b"kts" => Some(Self::Kotlin),
            _ => None,
        }
    }

    #[must_use]
    pub fn canonical_extension(&self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("rs"),
            Self::Python => Some("py"),
            Self::TypeScript => Some("ts"),
            Self::JavaScript => Some("js"),
            Self::Go => Some("go"),
            Self::Java => Some("java"),
            Self::C => Some("c"),
            Self::Cpp => Some("cpp"),
            Self::CSharp => Some("cs"),
            Self::Ruby => Some("rb"),
            Self::Swift => Some("swift"),
            Self::Kotlin => Some("kt"),
            Self::Other(_) => None,
        }
    }

    #[must_use]
    pub const fn is_known(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

impl FromStr for Language {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "rust" => Ok(Self::Rust),
            "python" => Ok(Self::Python),
            "typescript" | "ts" => Ok(Self::TypeScript),
            "javascript" | "js" => Ok(Self::JavaScript),
            "go" | "golang" => Ok(Self::Go),
            "java" => Ok(Self::Java),
            "c" => Ok(Self::C),
            "c++" | "cpp" => Ok(Self::Cpp),
            "c#" | "csharp" => Ok(Self::CSharp),
            "ruby" => Ok(Self::Ruby),
            "swift" => Ok(Self::Swift),
            "kotlin" | "kt" => Ok(Self::Kotlin),
            other => Err(Error::UnsupportedLanguage(other.to_owned())),
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => f.write_str("Rust"),
            Self::Python => f.write_str("Python"),
            Self::TypeScript => f.write_str("TypeScript"),
            Self::JavaScript => f.write_str("JavaScript"),
            Self::Go => f.write_str("Go"),
            Self::Java => f.write_str("Java"),
            Self::C => f.write_str("C"),
            Self::Cpp => f.write_str("C++"),
            Self::CSharp => f.write_str("C#"),
            Self::Ruby => f.write_str("Ruby"),
            Self::Swift => f.write_str("Swift"),
            Self::Kotlin => f.write_str("Kotlin"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Symbol visibility: public, crate-visible, protected, or private.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Visibility {
    Public,
    Crate,
    Protected,
    #[default]
    Private,
}

impl Visibility {
    #[must_use]
    pub const fn is_exported(self) -> bool {
        matches!(self, Self::Public)
    }
}

/// 1-based source location: file path, line range, and column range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub col_start: usize,
    pub col_end: usize,
}

impl SourceLocation {
    #[must_use]
    pub fn line_only(file: PathBuf, line: usize) -> Self {
        debug_assert!(line >= 1, "line must be >= 1");
        Self {
            file,
            line_start: line,
            line_end: line,
            col_start: 1,
            col_end: 1,
        }
    }
}

impl PartialOrd for SourceLocation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SourceLocation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.file
            .cmp(&other.file)
            .then(self.line_start.cmp(&other.line_start))
            .then(self.col_start.cmp(&other.col_start))
            .then(self.line_end.cmp(&other.line_end))
            .then(self.col_end.cmp(&other.col_end))
    }
}

/// A function or method declaration extracted from source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionDef {
    pub name: String,
    pub visibility: Visibility,
    pub signature: String,
    pub docstring: Option<String>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub body_stripped: bool,
    pub loc: SourceLocation,
}

impl FunctionDef {
    #[must_use]
    pub fn new(name: impl Into<String>, signature: impl Into<String>, loc: SourceLocation) -> Self {
        Self {
            name: name.into(),
            visibility: Visibility::default(),
            signature: signature.into(),
            docstring: None,
            parameters: Vec::new(),
            return_type: None,
            body_stripped: false,
            loc,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub name: String,
    pub type_annotation: Option<String>,
}

impl Parameter {
    #[must_use]
    pub fn new(name: impl Into<String>, type_annotation: Option<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            type_annotation: type_annotation.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructDef {
    pub name: String,
    pub visibility: Visibility,
    pub fields: Vec<Field>,
    pub derives: Vec<String>,
    pub docstring: Option<String>,
    pub body_stripped: bool,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClassDef {
    pub name: String,
    pub visibility: Visibility,
    pub extends: Option<String>,
    pub implements: Vec<String>,
    pub methods: Vec<FunctionDef>,
    pub fields: Vec<Field>,
    pub docstring: Option<String>,
    pub body_stripped: bool,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InterfaceDef {
    pub name: String,
    pub visibility: Visibility,
    pub extends: Vec<String>,
    pub methods: Vec<FunctionDef>,
    pub docstring: Option<String>,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumDef {
    pub name: String,
    pub visibility: Visibility,
    pub variants: Vec<(String, Option<String>)>,
    pub derives: Vec<String>,
    pub docstring: Option<String>,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeAliasDef {
    pub name: String,
    pub visibility: Visibility,
    pub target: String,
    pub generics: Vec<String>,
    pub docstring: Option<String>,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleDef {
    pub name: String,
    pub visibility: Visibility,
    pub docstring: Option<String>,
    pub loc: SourceLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    pub name: String,
    pub visibility: Visibility,
    pub type_annotation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Symbol {
    Function(FunctionDef),
    Struct(StructDef),
    Class(ClassDef),
    Interface(InterfaceDef),
    Enum(EnumDef),
    TypeAlias(TypeAliasDef),
    Module(ModuleDef),
}

macro_rules! from_def_for_symbol {
    ($variant:ident, $type:ty) => {
        impl From<$type> for Symbol {
            fn from(v: $type) -> Self {
                Self::$variant(v)
            }
        }
    };
}

from_def_for_symbol!(Function, FunctionDef);
from_def_for_symbol!(Struct, StructDef);
from_def_for_symbol!(Class, ClassDef);
from_def_for_symbol!(Interface, InterfaceDef);
from_def_for_symbol!(Enum, EnumDef);
from_def_for_symbol!(TypeAlias, TypeAliasDef);
from_def_for_symbol!(Module, ModuleDef);

/// Projects each [`Symbol`] variant into one of its inner type's fields without
/// repeating the 7-arm match in every accessor.
macro_rules! symbol_access {
    ($self:expr, $d:ident, $expr:expr) => {
        match $self {
            Symbol::Function($d) => $expr,
            Symbol::Struct($d) => $expr,
            Symbol::Class($d) => $expr,
            Symbol::Interface($d) => $expr,
            Symbol::Enum($d) => $expr,
            Symbol::TypeAlias($d) => $expr,
            Symbol::Module($d) => $expr,
        }
    };
}

impl Symbol {
    #[must_use]
    pub fn name(&self) -> &str {
        symbol_access!(self, d, &d.name)
    }

    #[must_use]
    pub fn visibility(&self) -> Visibility {
        symbol_access!(self, d, d.visibility)
    }

    #[must_use]
    pub fn location(&self) -> &SourceLocation {
        symbol_access!(self, d, &d.loc)
    }

    #[must_use]
    pub fn docstring(&self) -> Option<&str> {
        symbol_access!(self, d, d.docstring.as_deref())
    }

    #[must_use]
    pub fn is_stripped(&self) -> bool {
        match self {
            Self::Function(d) => d.body_stripped,
            Self::Struct(d) => d.body_stripped,
            Self::Class(d) => d.body_stripped,
            Self::Interface(_) | Self::Enum(_) | Self::TypeAlias(_) | Self::Module(_) => false,
        }
    }

    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Function(_) => "function",
            Self::Struct(_) => "struct",
            Self::Class(_) => "class",
            Self::Interface(_) => "interface",
            Self::Enum(_) => "enum",
            Self::TypeAlias(_) => "type alias",
            Self::Module(_) => "module",
        }
    }
}

impl PartialOrd for Symbol {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Symbol {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.kind_label()
            .cmp(other.kind_label())
            .then(self.name().cmp(other.name()))
            .then(self.location().cmp(other.location()))
    }
}

/// A single module-level import found in a source file.
///
/// `module` is the *raw* specifier exactly as written (`./user`, `os.path`,
/// `crate::ast`, `github.com/org/pkg`). Resolution to a real path happens later,
/// in `graph::resolve`, because it needs the full repository file set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ImportRef {
    /// Raw module specifier as written in the source.
    pub module: String,
    /// Named bindings pulled from the module. Empty means whole-module or wildcard.
    pub symbols: Vec<String>,
    /// 1-based line the import appears on.
    pub line: usize,
}

impl ImportRef {
    #[must_use]
    pub fn new(module: impl Into<String>, symbols: Vec<String>, line: usize) -> Self {
        Self {
            module: module.into(),
            symbols,
            line,
        }
    }
}

/// A single call site found in a source file.
///
/// `callee` is the bare called name. For method and attribute calls
/// (`obj.method()`, `pkg.Func()`) only the final segment is recorded, since that
/// is what can be matched against a symbol name without full type inference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CallRef {
    /// Final segment of the called expression.
    pub callee: String,
    /// Optional receiver or namespace qualifier (`obj` in `obj.method()`).
    pub qualifier: Option<String>,
    /// 1-based line the call appears on.
    pub line: usize,
    /// Name of the function or method that lexically encloses this call, if
    /// any. Calls at module level (not inside any function) have `None`.
    /// Used by the graph builder to create precise call edges from the
    /// enclosing function to the callee, rather than fanning out from every
    /// exported symbol in the file.
    pub enclosing_symbol: Option<String>,
}

impl CallRef {
    /// Creates a `CallRef` with an explicit enclosing symbol name.
    #[must_use]
    pub fn with_enclosing(
        callee: impl Into<String>,
        qualifier: Option<String>,
        line: usize,
        enclosing_symbol: Option<String>,
    ) -> Self {
        Self {
            callee: callee.into(),
            qualifier,
            line,
            enclosing_symbol,
        }
    }
}

/// Raw cross-file reference material extracted from one file.
///
/// This is the input to the dependency graph builder. It is kept separate from
/// [`Symbol`] because references are edges, not declarations.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileRefs {
    pub imports: Vec<ImportRef>,
    pub calls: Vec<CallRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileNode {
    pub path: PathBuf,
    pub language: Language,
    pub source: String,
    pub symbols: Vec<Symbol>,
    pub token_count: usize,
    pub has_redactions: bool,
    /// Module imports declared by this file. Populated in Phase 2.
    pub imports: Vec<ImportRef>,
    /// Call sites found in this file. Populated in Phase 2.
    pub calls: Vec<CallRef>,
}

impl FileNode {
    #[must_use]
    pub fn new(path: PathBuf, language: Language, source: String) -> Self {
        Self {
            path,
            language,
            source,
            symbols: Vec::new(),
            token_count: 0,
            has_redactions: false,
            imports: Vec::new(),
            calls: Vec::new(),
        }
    }

    #[must_use]
    pub fn directory(&self) -> &std::path::Path {
        self.path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_extension_lowercase() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
    }

    #[test]
    fn from_extension_mixed_case() {
        assert_eq!(Language::from_extension("Hpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("Tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("kTs"), Some(Language::Kotlin));
    }

    #[test]
    fn from_extension_unknown() {
        assert!(Language::from_extension("nonsense").is_none());
    }

    #[test]
    fn canonical_extension_known() {
        assert_eq!(Language::Rust.canonical_extension(), Some("rs"));
        assert_eq!(Language::Other("zig".into()).canonical_extension(), None);
    }

    #[test]
    fn language_from_str() {
        assert_eq!("rust".parse::<Language>().unwrap(), Language::Rust);
        assert_eq!("c++".parse::<Language>().unwrap(), Language::Cpp);
        assert!("nope".parse::<Language>().is_err());
    }

    #[test]
    fn language_is_known() {
        assert!(Language::Rust.is_known());
        assert!(!Language::Other("zig".into()).is_known());
    }

    #[test]
    fn visibility_default_and_exported() {
        assert_eq!(Visibility::default(), Visibility::Private);
        assert!(Visibility::Public.is_exported());
        assert!(!Visibility::Private.is_exported());
        assert!(!Visibility::Crate.is_exported());
        assert!(!Visibility::Protected.is_exported());
    }

    #[test]
    fn location_ordering() {
        let a = SourceLocation::line_only(PathBuf::from("a.rs"), 1);
        let b = SourceLocation::line_only(PathBuf::from("a.rs"), 2);
        let c = SourceLocation::line_only(PathBuf::from("b.rs"), 1);
        assert!(a < b && b < c);
    }

    #[test]
    fn symbol_from_conversion() {
        let loc = SourceLocation::line_only(PathBuf::from("test.rs"), 1);
        let def = FunctionDef::new("f", "fn f()", loc);
        let sym: Symbol = def.into();
        assert_eq!(sym.name(), "f");
        assert_eq!(sym.kind_label(), "function");
    }

    #[test]
    fn symbol_accessors() {
        let loc = SourceLocation::line_only(PathBuf::from("test.rs"), 1);
        let def = FunctionDef {
            name: "connect".into(),
            visibility: Visibility::Public,
            signature: "fn connect()".into(),
            docstring: Some("docs".into()),
            parameters: vec![],
            return_type: None,
            body_stripped: true,
            loc,
        };
        let sym = Symbol::from(def);
        assert_eq!(sym.name(), "connect");
        assert_eq!(sym.visibility(), Visibility::Public);
        assert!(sym.is_stripped());
        assert_eq!(sym.docstring(), Some("docs"));
    }

    #[test]
    fn symbol_ordering() {
        let loc = SourceLocation::line_only(PathBuf::from("test.rs"), 1);
        let fa: Symbol = FunctionDef::new("alpha", "fn alpha()", loc.clone()).into();
        let fb: Symbol = FunctionDef::new("beta", "fn beta()", loc).into();
        assert!(fa < fb);

        let st: Symbol = Symbol::Struct(StructDef {
            name: "Alpha".into(),
            visibility: Visibility::default(),
            fields: vec![],
            derives: vec![],
            docstring: None,
            body_stripped: false,
            loc: SourceLocation::line_only(PathBuf::from("test.rs"), 1),
        });
        assert!(fa < st);
    }

    #[test]
    fn parameter_constructor() {
        let p = Parameter::new("x", Some("i32"));
        assert_eq!(p.name, "x");
        assert_eq!(p.type_annotation, Some("i32".into()));

        let q = Parameter::new("y", None::<String>);
        assert_eq!(q.name, "y");
        assert_eq!(q.type_annotation, None);
    }

    #[test]
    fn field_direct_construction() {
        let f = Field {
            name: "count".into(),
            visibility: Visibility::Private,
            type_annotation: Some("usize".into()),
        };
        assert_eq!(f.name, "count");
        assert_eq!(f.visibility, Visibility::Private);
        assert_eq!(f.type_annotation, Some("usize".into()));
    }

    #[test]
    fn file_node() {
        let node = FileNode::new(
            PathBuf::from("src/main.rs"),
            Language::Rust,
            "fn main() {}".into(),
        );
        assert_eq!(node.directory(), std::path::Path::new("src"));
        assert_eq!(node.token_count, 0);
        assert!(!node.has_redactions);

        let root = FileNode::new(
            PathBuf::from("README.md"),
            Language::Other("md".into()),
            String::new(),
        );
        assert_eq!(root.directory(), std::path::Path::new("."));
    }
}
