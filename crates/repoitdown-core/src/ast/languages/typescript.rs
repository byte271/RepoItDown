use std::path::Path;

use crate::ast::SymbolExtractor;
use crate::ast::common::{
    enclosing_function_name, parse_source, strip_quotes, ts_loc, ts_text, visit_nodes,
};
use crate::error::Result;
use crate::types::{
    CallRef, ClassDef, EnumDef, FileRefs, FunctionDef, ImportRef, InterfaceDef, Language, Symbol,
    TypeAliasDef, Visibility,
};

#[derive(Default)]
pub struct TypeScriptExtractor;

impl SymbolExtractor for TypeScriptExtractor {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>> {
        let tree = parse_source(
            &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            source,
            path,
        )?;
        Ok(walk_ts_tree(tree.root_node(), source, path))
    }

    fn extract_refs(&self, source: &str, path: &Path) -> Result<FileRefs> {
        let tree = parse_source(
            &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            source,
            path,
        )?;
        let mut refs = FileRefs::default();

        visit_nodes(tree.root_node(), &mut |node| match node.kind() {
            IMPORT_STATEMENT => {
                collect_import(node, source, &mut refs.imports);
                false
            }
            // `export { x } from "./mod"` is a re-export: an import plus an export.
            EXPORT_STATEMENT if node.child_by_field_name("source").is_some() => {
                collect_import(node, source, &mut refs.imports);
                true
            }
            CALL_EXPRESSION => {
                if let Some(import) = require_import(node, source) {
                    refs.imports.push(import);
                } else if let Some(call) = ts_call(node, source) {
                    refs.calls.push(call);
                }
                // else: neither require nor a tracked call — nothing to record
                true
            }
            _ => true,
        });

        Ok(refs)
    }
}

/// Node kind for an `import ... from "..."` statement.
const IMPORT_STATEMENT: &str = "import_statement";
/// Node kind for a call expression.
const CALL_EXPRESSION: &str = "call_expression";
/// The legacy `require("...")` module loader.
const REQUIRE: &str = "require";
/// Node kinds that represent function or method declarations in the
/// TypeScript tree-sitter grammar.
const TS_FUNCTION_KINDS: &[&str] = &["function_declaration", "method_definition"];

/// Reads the module specifier from an import or re-export statement.
fn module_specifier(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let source_node = node
        .child_by_field_name("source")
        .or_else(|| first_child_of_kind(node, "string"))?;
    let text = first_child_of_kind(source_node, "string_fragment").map_or_else(
        || strip_quotes(ts_text(source_node, source)),
        |f| ts_text(f, source),
    );

    (!text.is_empty()).then(|| text.to_owned())
}

/// Finds the first direct child of the given kind.
fn first_child_of_kind<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

/// Collects the bound names from an `import_clause` or `export_clause`.
fn collect_bindings(node: tree_sitter::Node<'_>, source: &str, names: &mut Vec<String>) {
    visit_nodes(node, &mut |child| {
        match child.kind() {
            // `import { a, b as c }` / `export { a } from`
            "import_specifier" | "export_specifier" => {
                if let Some(name) = child.child_by_field_name("name") {
                    names.push(ts_text(name, source).to_owned());
                }
                return false;
            }
            // `import * as ns` binds the whole module, not a named symbol.
            "namespace_import" | "namespace_export" => return false,
            // `import Default from "..."`
            "identifier" => names.push(ts_text(child, source).to_owned()),
            _ => {}
        }
        true
    });
}

/// Collects a TypeScript import (or re-export) into an [`ImportRef`].
fn collect_import(node: tree_sitter::Node<'_>, source: &str, out: &mut Vec<ImportRef>) {
    let Some(module) = module_specifier(node, source) else {
        return;
    };
    let line = node.start_position().row + 1;

    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "import_clause" | "export_clause") {
            collect_bindings(child, source, &mut names);
        }
    }

    out.push(ImportRef::new(module, names, line));
}

/// Recognises a legacy `require("...")` call and turns it into an import.
fn require_import(node: tree_sitter::Node<'_>, source: &str) -> Option<ImportRef> {
    let target = node.child_by_field_name("function")?;
    if target.kind() != "identifier" || ts_text(target, source) != REQUIRE {
        return None;
    }

    let arguments = node.child_by_field_name("arguments")?;
    let literal = first_child_of_kind(arguments, "string")?;
    let module = first_child_of_kind(literal, "string_fragment").map_or_else(
        || strip_quotes(ts_text(literal, source)),
        |f| ts_text(f, source),
    );

    (!module.is_empty()).then(|| ImportRef::new(module, Vec::new(), node.start_position().row + 1))
}

/// Reads the callee out of a TypeScript `call_expression`.
fn ts_call(node: tree_sitter::Node<'_>, source: &str) -> Option<CallRef> {
    let target = node.child_by_field_name("function")?;
    let line = node.start_position().row + 1;
    let enclosing = enclosing_function_name(node, source, TS_FUNCTION_KINDS);

    match target.kind() {
        "identifier" => Some(CallRef::with_enclosing(
            ts_text(target, source),
            None,
            line,
            enclosing,
        )),
        "member_expression" => {
            let property = target.child_by_field_name("property")?;
            let qualifier = target
                .child_by_field_name("object")
                .map(|o| ts_text(o, source).to_owned());
            Some(CallRef::with_enclosing(
                ts_text(property, source),
                qualifier,
                line,
                enclosing,
            ))
        }
        _ => None,
    }
}

fn walk_ts_tree(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };

        match child.kind() {
            "function_declaration" | "method_definition" => {
                if let Some(sym) = extract_fn(child, source, path) {
                    symbols.push(sym);
                }
            }
            "class_declaration" => {
                if let Some(sym) = extract_class(child, source, path) {
                    symbols.push(sym);
                }
            }
            "interface_declaration" => {
                if let Some(sym) = extract_interface(child, source, path) {
                    symbols.push(sym);
                }
            }
            "type_alias_declaration" => {
                if let Some(sym) = extract_type_alias(child, source, path) {
                    symbols.push(sym);
                }
            }
            "enum_declaration" => {
                if let Some(sym) = extract_enum(child, source, path) {
                    symbols.push(sym);
                }
            }
            _ => {
                symbols.extend(walk_ts_tree(child, source, path));
            }
        }
    }

    symbols
}

/// Node kind that wraps any `export`ed declaration in the TypeScript grammar.
const EXPORT_STATEMENT: &str = "export_statement";
/// Node kind carrying an explicit `public`/`private`/`protected` class-member modifier.
const ACCESSIBILITY_MODIFIER: &str = "accessibility_modifier";
/// Node kind that terminates the upward search for an enclosing `export`.
const STATEMENT_BLOCK: &str = "statement_block";
/// How many ancestors to inspect when looking for an enclosing `export_statement`.
///
/// A class method needs three hops (`method_definition` -> `class_body` ->
/// `class_declaration` -> `export_statement`), so four gives a safe margin.
const EXPORT_LOOKUP_DEPTH: u8 = 4;

/// Reads an explicit TypeScript accessibility modifier, if the declaration has one.
fn accessibility_modifier(node: tree_sitter::Node<'_>, source: &str) -> Option<Visibility> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == ACCESSIBILITY_MODIFIER {
            return Some(match ts_text(child, source) {
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                _ => Visibility::Public,
            });
        }
    }
    None
}

/// Returns `true` when the declaration is (transitively) wrapped in an `export_statement`.
///
/// The tree walker descends from the root, so by the time a declaration is
/// visited its `export` wrapper is an *ancestor*, never a child. The search stops
/// at a `statement_block` so declarations nested inside function bodies are not
/// mistaken for module-level exports.
fn is_exported(node: tree_sitter::Node<'_>) -> bool {
    let mut current = node.parent();
    let mut depth = 0_u8;

    while let Some(parent) = current {
        match parent.kind() {
            EXPORT_STATEMENT => return true,
            STATEMENT_BLOCK => return false,
            _ => {}
        }
        depth += 1;
        if depth >= EXPORT_LOOKUP_DEPTH {
            return false;
        }
        current = parent.parent();
    }

    false
}

/// Resolves the effective visibility of a TypeScript declaration.
///
/// An explicit accessibility modifier always wins; otherwise the declaration is
/// public exactly when it is exported.
fn ts_visibility(node: tree_sitter::Node<'_>, source: &str) -> Visibility {
    accessibility_modifier(node, source).unwrap_or_else(|| {
        if is_exported(node) {
            Visibility::Public
        } else {
            Visibility::Private
        }
    })
}

fn extract_fn(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);

    Some(Symbol::from(FunctionDef {
        name,
        visibility: ts_visibility(node, source),
        signature: String::new(),
        docstring: None,
        parameters: vec![],
        return_type: None,
        body_stripped: false,
        loc,
    }))
}

/// Node kind holding a class's `extends` / `implements` clauses.
const CLASS_HERITAGE: &str = "class_heritage";
/// Node kind holding an interface's `extends` clause.
const EXTENDS_TYPE_CLAUSE: &str = "extends_type_clause";
/// Node kind for a single method inside a class body.
const METHOD_DEFINITION: &str = "method_definition";

/// Collects the identifier-like names referenced inside a heritage clause.
fn heritage_names(clause: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut cursor = clause.walk();
    clause
        .children(&mut cursor)
        .filter(|c| {
            matches!(
                c.kind(),
                "identifier" | "type_identifier" | "generic_type" | "nested_type_identifier"
            )
        })
        .map(|c| ts_text(c, source).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Splits a class's heritage into the single `extends` base and the `implements` list.
fn class_heritage(node: tree_sitter::Node<'_>, source: &str) -> (Option<String>, Vec<String>) {
    let mut extends = None;
    let mut implements = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != CLASS_HERITAGE {
            continue;
        }
        let mut inner = child.walk();
        for clause in child.children(&mut inner) {
            match clause.kind() {
                "extends_clause" => {
                    let mut names = heritage_names(clause, source);
                    if extends.is_none() && !names.is_empty() {
                        extends = Some(names.remove(0));
                    }
                    implements.extend(names);
                }
                "implements_clause" => implements.extend(heritage_names(clause, source)),
                _ => {}
            }
        }
    }

    (extends, implements)
}

/// Extracts the methods declared directly in a class body.
fn class_methods(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Vec<FunctionDef> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };

    let mut cursor = body.walk();
    body.children(&mut cursor)
        .filter(|c| c.kind() == METHOD_DEFINITION)
        .filter_map(|c| match extract_fn(c, source, path) {
            Some(Symbol::Function(f)) => Some(f),
            _ => None,
        })
        .collect()
}

fn extract_class(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);
    let (extends, implements) = class_heritage(node, source);

    Some(Symbol::from(ClassDef {
        name,
        visibility: ts_visibility(node, source),
        extends,
        implements,
        methods: class_methods(node, source, path),
        fields: vec![],
        docstring: None,
        body_stripped: false,
        loc,
    }))
}

fn extract_interface(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);

    let mut cursor = node.walk();
    let extends: Vec<String> = node
        .children(&mut cursor)
        .filter(|c| c.kind() == EXTENDS_TYPE_CLAUSE)
        .flat_map(|c| heritage_names(c, source))
        .collect();

    Some(Symbol::from(InterfaceDef {
        name,
        visibility: ts_visibility(node, source),
        extends,
        methods: vec![],
        docstring: None,
        loc,
    }))
}

fn extract_type_alias(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);

    Some(Symbol::from(TypeAliasDef {
        name,
        visibility: ts_visibility(node, source),
        target: String::new(),
        generics: vec![],
        docstring: None,
        loc,
    }))
}

fn extract_enum(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let loc = ts_loc(node, path);

    Some(Symbol::from(EnumDef {
        name,
        visibility: ts_visibility(node, source),
        variants: vec![],
        derives: vec![],
        docstring: None,
        loc,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function() {
        let source = "function greet(name: string): string {\n  return `Hello, ${name}`;\n}";
        let syms = TypeScriptExtractor
            .extract(source, Path::new("app.ts"))
            .unwrap();
        assert_eq!(syms[0].name(), "greet");
        assert_eq!(syms[0].kind_label(), "function");
    }

    #[test]
    fn parses_class() {
        let source =
            "class User {\n  name: string;\n  constructor(n: string) { this.name = n; }\n}";
        let syms = TypeScriptExtractor
            .extract(source, Path::new("user.ts"))
            .unwrap();
        let class_sym = syms.iter().find(|s| s.kind_label() == "class").unwrap();
        assert_eq!(class_sym.name(), "User");
    }

    #[test]
    fn parses_interface() {
        let source = "interface Repository<T> {\n  find(id: string): T;\n}";
        let syms = TypeScriptExtractor
            .extract(source, Path::new("repo.ts"))
            .unwrap();
        assert_eq!(syms[0].name(), "Repository");
        assert_eq!(syms[0].kind_label(), "interface");
    }

    /// Looks up a symbol's visibility, searching top-level symbols and class methods.
    fn visibility_of(source: &str, name: &str) -> Visibility {
        let syms = TypeScriptExtractor
            .extract(source, Path::new("mod.ts"))
            .unwrap();

        if let Some(sym) = syms.iter().find(|s| s.name() == name) {
            return sym.visibility();
        }

        syms.iter()
            .filter_map(|s| match s {
                Symbol::Class(c) => Some(c),
                _ => None,
            })
            .flat_map(|c| c.methods.iter())
            .find(|m| m.name == name)
            .map_or_else(|| panic!("symbol {name} not extracted"), |m| m.visibility)
    }

    #[test]
    fn exported_function_is_public() {
        assert_eq!(
            visibility_of("export function foo() {}", "foo"),
            Visibility::Public
        );
    }

    #[test]
    fn exported_class_and_interface_are_public() {
        assert_eq!(
            visibility_of("export class Bar {}", "Bar"),
            Visibility::Public
        );
        assert_eq!(
            visibility_of("export interface Baz { id: string }", "Baz"),
            Visibility::Public
        );
    }

    #[test]
    fn exported_type_alias_and_enum_are_public() {
        assert_eq!(
            visibility_of("export type Id = string;", "Id"),
            Visibility::Public
        );
        assert_eq!(
            visibility_of("export enum Color { Red }", "Color"),
            Visibility::Public
        );
    }

    #[test]
    fn default_export_is_public() {
        assert_eq!(
            visibility_of("export default function handler() {}", "handler"),
            Visibility::Public
        );
    }

    #[test]
    fn unexported_declaration_is_private() {
        assert_eq!(
            visibility_of("function hidden() {}", "hidden"),
            Visibility::Private
        );
        assert_eq!(
            visibility_of("class Hidden {}", "Hidden"),
            Visibility::Private
        );
    }

    #[test]
    fn accessibility_modifier_overrides_export() {
        let source = "export class Svc {\n  private secret() {}\n  protected hook() {}\n  public api() {}\n}";
        assert_eq!(visibility_of(source, "secret"), Visibility::Private);
        assert_eq!(visibility_of(source, "hook"), Visibility::Protected);
        assert_eq!(visibility_of(source, "api"), Visibility::Public);
    }

    #[test]
    fn method_of_exported_class_is_public() {
        let source = "export class Svc {\n  run() {}\n}";
        assert_eq!(visibility_of(source, "run"), Visibility::Public);
    }

    #[test]
    fn captures_class_heritage() {
        let source = "export class Admin extends User implements Serializable, Cloneable {}";
        let syms = TypeScriptExtractor
            .extract(source, Path::new("admin.ts"))
            .unwrap();
        let Some(Symbol::Class(class)) = syms.first() else {
            panic!("expected a class symbol");
        };
        assert_eq!(class.extends.as_deref(), Some("User"));
        assert_eq!(class.implements, vec!["Serializable", "Cloneable"]);
    }

    #[test]
    fn captures_interface_heritage() {
        let source = "export interface Admin extends User, Auditable { id: string }";
        let syms = TypeScriptExtractor
            .extract(source, Path::new("admin.ts"))
            .unwrap();
        let Some(Symbol::Interface(iface)) = syms.first() else {
            panic!("expected an interface symbol");
        };
        assert_eq!(iface.extends, vec!["User", "Auditable"]);
    }
}
