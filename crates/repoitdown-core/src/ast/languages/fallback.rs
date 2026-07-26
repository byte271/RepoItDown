use std::path::Path;

use regex::Regex;
use std::sync::LazyLock;

use crate::ast::SymbolExtractor;
use crate::error::Result;
use crate::types::{FunctionDef, Language, SourceLocation, Symbol};

static PATTERNS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    [
        (r"(?m)^\s*(?:pub\s+)?fn\s+(\w+)", "function"),
        (
            r"(?m)^\s*(?:pub\s+)?func\s+(?:\(\w+\s+\*?\w+\)\s+)?(\w+)",
            "function",
        ),
        (r"(?m)^\s*def\s+(\w+)", "function"),
        (
            r"(?m)^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)",
            "function",
        ),
        (r"(?m)^\s*(?:pub\s+)?struct\s+(\w+)", "struct"),
        (r"(?m)^\s*(?:pub\s+)?enum\s+(\w+)", "enum"),
        (r"(?m)^\s*(?:pub\s+)?trait\s+(\w+)", "interface"),
        (
            r"(?m)^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)",
            "class",
        ),
        (r"(?m)^\s*(?:export\s+)?interface\s+(\w+)", "interface"),
        (r"(?m)^\s*(?:pub\s+)?type\s+(\w+)", "type alias"),
        (r"(?m)^\s*(?:export\s+)?type\s+(\w+)", "type alias"),
        (r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)", "module"),
    ]
    .iter()
    .map(|(p, kind)| {
        (
            Regex::new(p).expect("built-in fallback regex failed to compile"),
            *kind,
        )
    })
    .collect()
});

pub struct FallbackExtractor;

impl SymbolExtractor for FallbackExtractor {
    fn language(&self) -> Language {
        Language::Other(Language::UNKNOWN.into())
    }

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>> {
        let mut symbols = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (regex, kind) in PATTERNS.iter() {
            for caps in regex.captures_iter(source) {
                let name = &caps[1];
                if !seen.insert((*kind, name.to_string())) {
                    continue;
                }

                let pos = caps.get(0).unwrap().start();
                let line = source[..pos].chars().filter(|&c| c == '\n').count() + 1;
                let loc = SourceLocation::line_only(path.to_path_buf(), line);

                if let Some(sym) = build_symbol(kind, name, loc) {
                    symbols.push(sym);
                }
            }
        }

        symbols.sort_by(|a, b| a.location().cmp(b.location()));
        Ok(symbols)
    }

    fn is_available(&self) -> bool {
        true
    }
}

fn build_symbol(kind: &str, name: &str, loc: SourceLocation) -> Option<Symbol> {
    match kind {
        "function" => Some(Symbol::from(FunctionDef::new(name, String::new(), loc))),
        "class" => Some(Symbol::from(crate::types::ClassDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            extends: None,
            implements: vec![],
            methods: vec![],
            fields: vec![],
            docstring: None,
            body_stripped: true,
            loc,
        })),
        "struct" => Some(Symbol::from(crate::types::StructDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            fields: vec![],
            derives: vec![],
            docstring: None,
            body_stripped: true,
            loc,
        })),
        "enum" => Some(Symbol::from(crate::types::EnumDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            variants: vec![],
            derives: vec![],
            docstring: None,
            loc,
        })),
        "interface" => Some(Symbol::from(crate::types::InterfaceDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            extends: vec![],
            methods: vec![],
            docstring: None,
            loc,
        })),
        "type alias" => Some(Symbol::from(crate::types::TypeAliasDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            target: String::new(),
            generics: vec![],
            docstring: None,
            loc,
        })),
        "module" => Some(Symbol::from(crate::types::ModuleDef {
            name: name.to_string(),
            visibility: crate::types::Visibility::default(),
            docstring: None,
            loc,
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_functions() {
        let source = "fn main() {}\npub fn helper() -> i32 { 42 }";
        let syms = FallbackExtractor
            .extract(source, Path::new("test.rs"))
            .unwrap();
        let names: Vec<&str> = syms.iter().map(Symbol::name).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"helper"));
    }

    #[test]
    fn detects_python() {
        let source = "def foo():\n    pass\n\ndef bar(x, y):\n    return x + y";
        let syms = FallbackExtractor
            .extract(source, Path::new("test.py"))
            .unwrap();
        let names: Vec<&str> = syms.iter().map(Symbol::name).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn detects_go() {
        let source = "func main() {\n}\n\nfunc (s *Server) Start() error {\n    return nil\n}";
        let syms = FallbackExtractor
            .extract(source, Path::new("test.go"))
            .unwrap();
        let names: Vec<&str> = syms.iter().map(Symbol::name).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"Start"));
    }

    #[test]
    fn detects_classes_and_interfaces() {
        let source = "class User {\n}\n\ninterface Repository {\n}\n\nstruct Config {\n}";
        let syms = FallbackExtractor
            .extract(source, Path::new("test.ts"))
            .unwrap();
        let kinds: Vec<&str> = syms.iter().map(Symbol::kind_label).collect();
        assert!(kinds.contains(&"class"));
        assert!(kinds.contains(&"interface"));
        assert!(kinds.contains(&"struct"));
    }
}
