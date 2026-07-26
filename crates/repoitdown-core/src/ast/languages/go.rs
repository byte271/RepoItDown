use std::path::Path;

use crate::ast::SymbolExtractor;
use crate::ast::common::{
    enclosing_function_name, parse_source, strip_quotes, ts_loc, ts_text, visit_nodes,
};
use crate::error::Result;
use crate::types::{
    CallRef, FileRefs, FunctionDef, ImportRef, InterfaceDef, Language, StructDef, Symbol,
    TypeAliasDef, Visibility,
};

#[derive(Default)]
pub struct GoExtractor;

impl SymbolExtractor for GoExtractor {
    fn language(&self) -> Language {
        Language::Go
    }

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>> {
        let tree = parse_source(&tree_sitter_go::LANGUAGE.into(), source, path)?;
        Ok(walk_go_tree(tree.root_node(), source, path))
    }

    fn extract_refs(&self, source: &str, path: &Path) -> Result<FileRefs> {
        let tree = parse_source(&tree_sitter_go::LANGUAGE.into(), source, path)?;
        let mut refs = FileRefs::default();

        visit_nodes(tree.root_node(), &mut |node| match node.kind() {
            IMPORT_DECLARATION => {
                collect_imports(node, source, &mut refs.imports);
                false
            }
            CALL_EXPRESSION => {
                if let Some(call) = go_call(node, source) {
                    refs.calls.push(call);
                }
                true
            }
            _ => true,
        });

        Ok(refs)
    }
}

/// Node kind for an `import` block or single import.
const IMPORT_DECLARATION: &str = "import_declaration";
/// Node kind for a single import inside an `import` declaration.
const IMPORT_SPEC: &str = "import_spec";
/// Node kind for a call expression.
const CALL_EXPRESSION: &str = "call_expression";
/// Node kinds that represent function or method declarations in the Go
/// tree-sitter grammar.
const GO_FUNCTION_KINDS: &[&str] = &["function_declaration", "method_declaration"];

/// Collects every `import_spec` inside an import declaration.
///
/// Handles both the single form (`import "fmt"`) and the parenthesised list.
fn collect_imports(node: tree_sitter::Node<'_>, source: &str, out: &mut Vec<ImportRef>) {
    visit_nodes(node, &mut |child| {
        if child.kind() != IMPORT_SPEC {
            return true;
        }
        if let Some(path_node) = child.child_by_field_name("path") {
            let module = strip_quotes(ts_text(path_node, source));
            if !module.is_empty() {
                out.push(ImportRef::new(
                    module,
                    Vec::new(),
                    child.start_position().row + 1,
                ));
            }
        }
        false
    });
}

/// Reads the callee out of a Go `call_expression`.
fn go_call(node: tree_sitter::Node<'_>, source: &str) -> Option<CallRef> {
    let target = node.child_by_field_name("function")?;
    let line = node.start_position().row + 1;
    let enclosing = enclosing_function_name(node, source, GO_FUNCTION_KINDS);

    match target.kind() {
        "identifier" => Some(CallRef::with_enclosing(
            ts_text(target, source),
            None,
            line,
            enclosing,
        )),
        "selector_expression" => {
            let field = target.child_by_field_name("field")?;
            let qualifier = target
                .child_by_field_name("operand")
                .map(|o| ts_text(o, source).to_owned());
            Some(CallRef::with_enclosing(
                ts_text(field, source),
                qualifier,
                line,
                enclosing,
            ))
        }
        _ => None,
    }
}

fn walk_go_tree(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };

        match child.kind() {
            "function_declaration" | "method_declaration" => {
                if let Some(sym) = extract_fn(child, source, path) {
                    symbols.push(sym);
                }
            }
            "type_declaration" => {
                for j in 0..child.child_count() {
                    if let Some(spec) = child.child(j) {
                        if spec.kind() == "type_spec" {
                            if let Some(sym) = extract_type_spec(spec, source, path) {
                                symbols.push(sym);
                            }
                        }
                    }
                }
            }
            _ => {
                symbols.extend(walk_go_tree(child, source, path));
            }
        }
    }

    symbols
}

/// Resolves Go visibility from an identifier.
///
/// Go has no visibility keywords: an identifier is exported from its package
/// exactly when its first character is an uppercase letter.
fn go_visibility(name: &str) -> Visibility {
    if name.chars().next().is_some_and(char::is_uppercase) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

fn extract_fn(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);
    let visibility = go_visibility(&name);

    Some(Symbol::from(FunctionDef {
        name,
        visibility,
        signature: String::new(),
        docstring: None,
        parameters: vec![],
        return_type: None,
        body_stripped: false,
        loc,
    }))
}

fn extract_type_spec(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);
    let visibility = go_visibility(&name);

    let type_node = node.child_by_field_name("type")?;
    match type_node.kind() {
        "struct_type" => Some(Symbol::from(StructDef {
            name,
            visibility,
            fields: vec![],
            derives: vec![],
            docstring: None,
            body_stripped: false,
            loc,
        })),
        "interface_type" => Some(Symbol::from(InterfaceDef {
            name,
            visibility,
            extends: vec![],
            methods: vec![],
            docstring: None,
            loc,
        })),
        _ => Some(Symbol::from(TypeAliasDef {
            name,
            visibility,
            target: ts_text(type_node, source).to_string(),
            generics: vec![],
            docstring: None,
            loc,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function() {
        let source = "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}";
        let syms = GoExtractor.extract(source, Path::new("main.go")).unwrap();
        assert_eq!(syms[0].name(), "main");
        assert_eq!(syms[0].kind_label(), "function");
    }

    #[test]
    fn parses_struct_type() {
        let source = "package models\n\ntype User struct {\n\tName string\n\tAge  int\n}";
        let syms = GoExtractor.extract(source, Path::new("user.go")).unwrap();
        assert_eq!(syms[0].name(), "User");
        assert_eq!(syms[0].kind_label(), "struct");
    }

    #[test]
    fn parses_interface_type() {
        let source =
            "package repo\n\ntype Repository interface {\n\tFind(id int) (*Entity, error)\n}";
        let syms = GoExtractor.extract(source, Path::new("repo.go")).unwrap();
        assert_eq!(syms[0].name(), "Repository");
        assert_eq!(syms[0].kind_label(), "interface");
    }

    #[test]
    fn parses_method() {
        let source = "package server\n\nfunc (s *Server) Start() error {\n\treturn nil\n}";
        let syms = GoExtractor.extract(source, Path::new("server.go")).unwrap();
        assert_eq!(syms[0].name(), "Start");
    }

    fn visibility_of(source: &str, name: &str) -> Visibility {
        let syms = GoExtractor.extract(source, Path::new("pkg.go")).unwrap();
        let Some(sym) = syms.iter().find(|s| s.name() == name) else {
            panic!("symbol {name} not extracted");
        };
        sym.visibility()
    }

    #[test]
    fn uppercase_function_is_public() {
        let source = "package p\n\nfunc Exported() {}\n\nfunc unexported() {}";
        assert_eq!(visibility_of(source, "Exported"), Visibility::Public);
        assert_eq!(visibility_of(source, "unexported"), Visibility::Private);
    }

    #[test]
    fn uppercase_types_are_public() {
        let source = "package p\n\ntype User struct{}\n\ntype internal struct{}\n\ntype Reader interface{}\n\ntype ID int";
        assert_eq!(visibility_of(source, "User"), Visibility::Public);
        assert_eq!(visibility_of(source, "internal"), Visibility::Private);
        assert_eq!(visibility_of(source, "Reader"), Visibility::Public);
        assert_eq!(visibility_of(source, "ID"), Visibility::Public);
    }

    #[test]
    fn uppercase_method_is_public() {
        let source = "package p\n\nfunc (s *S) Start() {}\n\nfunc (s *S) stop() {}";
        assert_eq!(visibility_of(source, "Start"), Visibility::Public);
        assert_eq!(visibility_of(source, "stop"), Visibility::Private);
    }
}
