use std::path::Path;

use crate::ast::common::{enclosing_function_name, parse_source, ts_loc, ts_text, visit_nodes};
use crate::ast::SymbolExtractor;
use crate::error::Result;
use crate::types::{
    CallRef, ClassDef, FileRefs, FunctionDef, ImportRef, Language, Parameter, Symbol,
    Visibility,
};

#[derive(Default)]
pub struct PythonExtractor;

impl SymbolExtractor for PythonExtractor {
    fn language(&self) -> Language {
        Language::Python
    }

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>> {
        let tree = parse_source(&tree_sitter_python::LANGUAGE.into(), source, path)?;
        Ok(walk_python_tree(tree.root_node(), source, path))
    }

    fn extract_refs(&self, source: &str, path: &Path) -> Result<FileRefs> {
        let tree = parse_source(&tree_sitter_python::LANGUAGE.into(), source, path)?;
        let mut refs = FileRefs::default();

        visit_nodes(tree.root_node(), &mut |node| match node.kind() {
            IMPORT_STATEMENT => {
                collect_plain_import(node, source, &mut refs.imports);
                false
            }
            IMPORT_FROM_STATEMENT => {
                collect_from_import(node, source, &mut refs.imports);
                false
            }
            CALL => {
                if let Some(call) = python_call(node, source) {
                    refs.calls.push(call);
                }
                true
            }
            _ => true,
        });

        Ok(refs)
    }
}

/// Node kind for `import a.b`.
const IMPORT_STATEMENT: &str = "import_statement";
/// Node kind for `from a.b import c`.
const IMPORT_FROM_STATEMENT: &str = "import_from_statement";
/// Node kind for a call expression.
const CALL: &str = "call";
/// Node kinds that represent function declarations in the Python tree-sitter
/// grammar.
const PYTHON_FUNCTION_KINDS: &[&str] = &["function_definition"];

/// Collects `import a.b` / `import a.b as c` specifiers.
fn collect_plain_import(
    node: tree_sitter::Node<'_>,
    source: &str,
    out: &mut Vec<ImportRef>,
) {
    let line = node.start_position().row + 1;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let target = match child.kind() {
            "dotted_name" => Some(child),
            "aliased_import" => child.child_by_field_name("name"),
            _ => None,
        };
        if let Some(target) = target {
            let module = ts_text(target, source);
            if !module.is_empty() {
                out.push(ImportRef::new(module, Vec::new(), line));
            }
        }
    }
}

/// Collects `from a.b import c, d` / `from . import x` / `from a import *`.
///
/// Relative imports keep their leading dots in `module` so the resolver can tell
/// `from .sibling import x` apart from an absolute package of the same name.
fn collect_from_import(
    node: tree_sitter::Node<'_>,
    source: &str,
    out: &mut Vec<ImportRef>,
) {
    let line = node.start_position().row + 1;
    let Some(module_node) = node.child_by_field_name("module_name") else {
        return;
    };
    let module = ts_text(module_node, source).to_owned();
    if module.is_empty() {
        return;
    }

    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.id() == module_node.id() {
            continue;
        }
        match child.kind() {
            "dotted_name" => names.push(ts_text(child, source).to_owned()),
            "aliased_import" => {
                if let Some(name) = child.child_by_field_name("name") {
                    names.push(ts_text(name, source).to_owned());
                }
            }
            // `from a import *` binds no nameable symbol.
            "wildcard_import" => names.clear(),
            _ => {}
        }
    }

    out.push(ImportRef::new(module, names, line));
}

/// Reads the callee out of a Python `call` node.
fn python_call(node: tree_sitter::Node<'_>, source: &str) -> Option<CallRef> {
    let target = node.child_by_field_name("function")?;
    let line = node.start_position().row + 1;
    let enclosing = enclosing_function_name(node, source, PYTHON_FUNCTION_KINDS);

    match target.kind() {
        "identifier" => Some(CallRef::with_enclosing(
            ts_text(target, source),
            None,
            line,
            enclosing,
        )),
        "attribute" => {
            let attribute = target.child_by_field_name("attribute")?;
            let qualifier = target
                .child_by_field_name("object")
                .map(|o| ts_text(o, source).to_owned());
            Some(CallRef::with_enclosing(
                ts_text(attribute, source),
                qualifier,
                line,
                enclosing,
            ))
        }
        _ => None,
    }
}

fn walk_python_tree(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };

        match child.kind() {
            "function_definition" => {
                if let Some(sym) = extract_fn(child, source, path) {
                    symbols.push(sym);
                }
            }
            "class_definition" => {
                if let Some(sym) = extract_class(child, source, path) {
                    symbols.push(sym);
                }
            }
            _ => {
                symbols.extend(walk_python_tree(child, source, path));
            }
        }
    }

    symbols
}

/// Resolves Python visibility from an identifier.
///
/// Python has no access keywords: the community convention (PEP 8) is that a
/// leading underscore marks a name as internal, everything else is public.
fn py_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

fn extract_fn(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let visibility = py_visibility(&name);

    let loc = ts_loc(node, path);

    let params = extract_params(node, source);
    let return_type = node
        .child_by_field_name("return_type")
        .map(|rt| ts_text(rt, source).to_string());

    let docstring = extract_docstring(node, source);

    Some(Symbol::from(FunctionDef {
        name,
        visibility,
        signature: format!(
            "def {}({})",
            name_node.utf8_text(source.as_bytes()).unwrap_or("?"),
            params_text(&params)
        ),
        docstring,
        parameters: params,
        return_type,
        body_stripped: false,
        loc,
    }))
}

fn extract_params(node: tree_sitter::Node<'_>, source: &str) -> Vec<Parameter> {
    let Some(params_node) = node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut params = Vec::new();

    for i in 0..params_node.child_count() {
        let Some(child) = params_node.child(i) else { continue };
        if child.kind() == "identifier" {
            let name = ts_text(child, source).to_string();
            let type_ann = child
                .child_by_field_name("type")
                .map(|t| ts_text(t, source).to_string());
            params.push(Parameter::new(name, type_ann));
        } else if child.kind() == "typed_parameter" || child.kind() == "typed_default_parameter" {
            if let Some(id) = child.child_by_field_name("name") {
                let name = ts_text(id, source).to_string();
                let type_ann = child
                    .child_by_field_name("type")
                    .map(|t| ts_text(t, source).to_string());
                params.push(Parameter::new(name, type_ann));
            }
        }
    }

    params
}

fn params_text(params: &[Parameter]) -> String {
    params
        .iter()
        .map(|p| {
            if let Some(ty) = &p.type_annotation {
                format!("{}: {ty}", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn extract_class(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let visibility = py_visibility(&name);

    let loc = ts_loc(node, path);

    let extends = extract_superclasses(node, source);
    let docstring = extract_docstring(node, source);

    let body = node.child_by_field_name("body");
    let methods: Vec<FunctionDef> = body
        .map(|b| {
            walk_python_tree(b, source, path)
                .into_iter()
                .filter_map(|s| match s {
                    Symbol::Function(f) => Some(f),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Symbol::from(ClassDef {
        name,
        visibility,
        extends,
        implements: vec![],
        methods,
        fields: vec![],
        docstring,
        body_stripped: false,
        loc,
    }))
}

fn extract_superclasses(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let supers = node.child_by_field_name("superclasses")?;
    let names: Vec<&str> = (0..supers.child_count())
        .filter_map(|i| supers.child(i))
        .filter(|c| c.kind() == "identifier")
        .map(|c| ts_text(c, source))
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn extract_docstring(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = body.child(0)?;
    if first.kind() == "expression_statement" {
        let expr = first.child(0)?;
        if expr.kind() == "string" {
            let text = ts_text(expr, source);
            let stripped = text.trim_matches('"').trim_matches('\'');
            if !stripped.is_empty() {
                return Some(stripped.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function() {
        let source = "def greet(name: str) -> str:\n    return f\"Hello, {name}\"";
        let syms = PythonExtractor
            .extract(source, Path::new("app.py"))
            .unwrap();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name(), "greet");
        assert_eq!(syms[0].kind_label(), "function");
    }

    #[test]
    fn parses_class_with_inheritance() {
        let source = "class Dog(Animal):\n    def bark(self):\n        pass";
        let syms = PythonExtractor
            .extract(source, Path::new("models.py"))
            .unwrap();
        let class_sym = syms.iter().find(|s| s.kind_label() == "class").unwrap();
        assert_eq!(class_sym.name(), "Dog");
    }

    #[test]
    fn parses_top_level_functions() {
        let source = "def foo():\n    pass\n\ndef bar():\n    pass";
        let syms = PythonExtractor
            .extract(source, Path::new("lib.py"))
            .unwrap();
        assert_eq!(syms.len(), 2);
    }

    fn visibility_of(source: &str, name: &str) -> Visibility {
        let syms = PythonExtractor
            .extract(source, Path::new("mod.py"))
            .unwrap();
        let Some(sym) = syms.iter().find(|s| s.name() == name) else {
            panic!("symbol {name} not extracted");
        };
        sym.visibility()
    }

    #[test]
    fn plain_name_is_public() {
        let source = "def pub():\n    pass";
        assert_eq!(visibility_of(source, "pub"), Visibility::Public);
    }

    #[test]
    fn underscore_name_is_private() {
        let source = "def _helper():\n    pass\n\ndef __mangled():\n    pass";
        assert_eq!(visibility_of(source, "_helper"), Visibility::Private);
        assert_eq!(visibility_of(source, "__mangled"), Visibility::Private);
    }

    #[test]
    fn class_visibility_follows_underscore_convention() {
        let source = "class Public:\n    pass\n\nclass _Internal:\n    pass";
        assert_eq!(visibility_of(source, "Public"), Visibility::Public);
        assert_eq!(visibility_of(source, "_Internal"), Visibility::Private);
    }
}
