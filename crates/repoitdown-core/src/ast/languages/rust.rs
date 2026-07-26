use std::path::Path;

use crate::ast::SymbolExtractor;
use crate::ast::common::{enclosing_function_name, parse_source, ts_loc, ts_text, visit_nodes};
use crate::error::Result;
use crate::types::{
    CallRef, EnumDef, Field, FileRefs, FunctionDef, ImportRef, InterfaceDef, Language, ModuleDef,
    StructDef, Symbol, TypeAliasDef, Visibility,
};

#[derive(Default)]
pub struct RustExtractor;

impl SymbolExtractor for RustExtractor {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn extract(&self, source: &str, path: &Path) -> Result<Vec<Symbol>> {
        let tree = parse_source(&tree_sitter_rust::LANGUAGE.into(), source, path)?;
        Ok(walk_rust_tree(tree.root_node(), source, path))
    }

    fn extract_refs(&self, source: &str, path: &Path) -> Result<FileRefs> {
        let tree = parse_source(&tree_sitter_rust::LANGUAGE.into(), source, path)?;
        let mut refs = FileRefs::default();

        visit_nodes(tree.root_node(), &mut |node| match node.kind() {
            USE_DECLARATION => {
                let line = node.start_position().row + 1;
                if let Some(argument) = node.child_by_field_name("argument") {
                    collect_use_tree(argument, source, "", line, &mut refs.imports);
                }
                false
            }
            CALL_EXPRESSION => {
                if let Some(call) = rust_call(node, source) {
                    refs.calls.push(call);
                }
                true
            }
            _ => true,
        });

        Ok(refs)
    }
}

/// Rust path separator.
const PATH_SEP: &str = "::";
/// Node kind for a `use ...;` statement.
const USE_DECLARATION: &str = "use_declaration";
/// Node kind for a function or method call.
const CALL_EXPRESSION: &str = "call_expression";

/// Joins a `use`-tree prefix with a relative path segment.
fn join_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_owned()
    } else {
        format!("{prefix}{PATH_SEP}{segment}")
    }
}

/// Records an import whose full path is `full`, splitting off the trailing name.
fn push_scoped(full: &str, line: usize, out: &mut Vec<ImportRef>) {
    if let Some((module, name)) = full.rsplit_once(PATH_SEP) {
        out.push(ImportRef::new(module, vec![name.to_owned()], line));
    } else if !full.is_empty() {
        out.push(ImportRef::new(full, Vec::new(), line));
    }
    // else: full is empty, nothing to push
}

/// Flattens a Rust `use` tree into one [`ImportRef`] per bound name.
///
/// Rust `use` syntax nests arbitrarily (`use a::{b::{c, d}, e as f, g::*}`), so
/// the tree is walked recursively while threading the accumulated module prefix.
fn collect_use_tree(
    node: tree_sitter::Node<'_>,
    source: &str,
    prefix: &str,
    line: usize,
    out: &mut Vec<ImportRef>,
) {
    match node.kind() {
        "scoped_identifier" => push_scoped(&join_path(prefix, ts_text(node, source)), line, out),
        "identifier" | "crate" | "super" | "self" | "metavariable" => {
            let text = ts_text(node, source);
            if text.is_empty() {
                return;
            }
            if prefix.is_empty() {
                out.push(ImportRef::new(text, Vec::new(), line));
            } else {
                out.push(ImportRef::new(prefix, vec![text.to_owned()], line));
            }
        }
        "use_wildcard" => {
            let text = ts_text(node, source);
            let module = text.trim_end_matches('*').trim_end_matches(':');
            let full = join_path(prefix, module);
            if !full.is_empty() {
                out.push(ImportRef::new(full, Vec::new(), line));
            }
        }
        "use_as_clause" => {
            if let Some(inner) = node.child_by_field_name("path") {
                collect_use_tree(inner, source, prefix, line, out);
            }
        }
        "scoped_use_list" => {
            let next = node.child_by_field_name("path").map_or_else(
                || prefix.to_owned(),
                |p| join_path(prefix, ts_text(p, source)),
            );
            if let Some(list) = node.child_by_field_name("list") {
                collect_use_tree(list, source, &next, line, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    collect_use_tree(child, source, prefix, line, out);
                }
            }
        }
        _ => {}
    }
}

/// Node kinds that represent function or method declarations in the Rust
/// tree-sitter grammar. Used by [`enclosing_function_name`] to find the
/// enclosing function for a call expression.
const RUST_FUNCTION_KINDS: &[&str] = &["function_item"];

/// Reads the callee out of a Rust `call_expression`.
fn rust_call(node: tree_sitter::Node<'_>, source: &str) -> Option<CallRef> {
    let mut target = node.child_by_field_name("function")?;
    if target.kind() == "generic_function" {
        target = target.child_by_field_name("function")?;
    }
    let line = node.start_position().row + 1;
    let enclosing = enclosing_function_name(node, source, RUST_FUNCTION_KINDS);

    match target.kind() {
        "identifier" => Some(CallRef::with_enclosing(
            ts_text(target, source),
            None,
            line,
            enclosing,
        )),
        "scoped_identifier" => {
            let text = ts_text(target, source);
            let (qualifier, name) = text
                .rsplit_once(PATH_SEP)
                .map_or((None, text), |(q, n)| (Some(q.to_owned()), n));
            Some(CallRef::with_enclosing(name, qualifier, line, enclosing))
        }
        "field_expression" => {
            let field = target.child_by_field_name("field")?;
            let qualifier = target
                .child_by_field_name("value")
                .map(|v| ts_text(v, source).to_owned());
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

fn walk_rust_tree(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Vec<Symbol> {
    let mut symbols = Vec::new();

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };

        match child.kind() {
            "function_item" => {
                if let Some(sym) = extract_fn(child, source, path) {
                    symbols.push(sym);
                }
            }
            "struct_item" => {
                if let Some(sym) = extract_struct(child, source, path) {
                    symbols.push(sym);
                }
            }
            "enum_item" | "trait_item" | "type_item" | "mod_item" => {
                let maybe_sym = match child.kind() {
                    "enum_item" => extract_enum(child, source, path),
                    "trait_item" => extract_trait(child, source, path),
                    "type_item" => extract_type_alias(child, source, path),
                    "mod_item" => extract_mod(child, source, path),
                    _ => None,
                };
                if let Some(sym) = maybe_sym {
                    symbols.push(sym);
                }
            }
            _ => {
                symbols.extend(walk_rust_tree(child, source, path));
            }
        }
    }

    symbols
}

fn extract_fn(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let vis = extract_visibility(node, source);

    let loc = ts_loc(node, path);

    let signature = node
        .child_by_field_name("parameters")
        .map_or("()", |p| ts_text(p, source));

    let return_type = node
        .child_by_field_name("return_type")
        .map(|rt| ts_text(rt, source).to_string());

    let docstring = extract_doc_comment(node, source);

    let signature_str = format!("fn {name}{signature}");
    Some(Symbol::from(FunctionDef {
        name,
        visibility: vis,
        signature: signature_str,
        docstring,
        parameters: vec![],
        return_type,
        body_stripped: false,
        loc,
    }))
}

fn extract_struct(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let vis = extract_visibility(node, source);

    let loc = ts_loc(node, path);

    let fields = extract_struct_fields(node, source);
    let derives = extract_derives(node, source);
    let docstring = extract_doc_comment(node, source);

    Some(Symbol::from(StructDef {
        name,
        visibility: vis,
        fields,
        derives,
        docstring,
        body_stripped: false,
        loc,
    }))
}

fn extract_struct_fields(node: tree_sitter::Node<'_>, source: &str) -> Vec<Field> {
    let Some(body) = node.child_by_field_name("body") else {
        return vec![];
    };
    let mut fields = Vec::new();

    for i in 0..body.child_count() {
        let Some(child) = body.child(i) else { continue };
        if child.kind() == "field_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = ts_text(name_node, source).to_string();
                let vis = extract_visibility(child, source);
                let type_ann = child
                    .child_by_field_name("type")
                    .map(|t| ts_text(t, source).to_string());
                fields.push(Field {
                    name,
                    visibility: vis,
                    type_annotation: type_ann,
                });
            }
        }
    }

    fields
}

fn extract_enum(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let vis = extract_visibility(node, source);

    let loc = ts_loc(node, path);

    let variants = extract_enum_variants(node, source);
    let derives = extract_derives(node, source);
    let docstring = extract_doc_comment(node, source);

    Some(Symbol::from(EnumDef {
        name,
        visibility: vis,
        variants,
        derives,
        docstring,
        loc,
    }))
}

fn extract_enum_variants(
    node: tree_sitter::Node<'_>,
    source: &str,
) -> Vec<(String, Option<String>)> {
    let Some(body) = node.child_by_field_name("body") else {
        return vec![];
    };
    let mut variants = Vec::new();

    for i in 0..body.child_count() {
        let Some(child) = body.child(i) else { continue };
        if child.kind() == "enum_variant" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = ts_text(name_node, source).to_string();
                variants.push((name, None));
            }
        }
    }

    variants
}

fn extract_trait(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let vis = extract_visibility(node, source);

    let loc = ts_loc(node, path);

    let docstring = extract_doc_comment(node, source);

    Some(Symbol::from(InterfaceDef {
        name,
        visibility: vis,
        extends: vec![],
        methods: vec![],
        docstring,
        loc,
    }))
}

fn extract_type_alias(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();
    let vis = extract_visibility(node, source);

    let loc = ts_loc(node, path);

    let target = node
        .child_by_field_name("type")
        .map(|t| ts_text(t, source).to_string())
        .unwrap_or_default();

    let docstring = extract_doc_comment(node, source);

    Some(Symbol::from(TypeAliasDef {
        name,
        visibility: vis,
        target,
        generics: vec![],
        docstring,
        loc,
    }))
}

fn extract_mod(node: tree_sitter::Node<'_>, source: &str, path: &Path) -> Option<Symbol> {
    let name_node = node.child_by_field_name("name")?;
    let name = ts_text(name_node, source).to_string();

    let loc = ts_loc(node, path);

    Some(Symbol::from(ModuleDef {
        name,
        visibility: Visibility::default(),
        docstring: None,
        loc,
    }))
}

fn extract_visibility(node: tree_sitter::Node<'_>, source: &str) -> Visibility {
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "visibility_modifier" => {
                let text = ts_text(child, source);
                if text.contains("pub(crate)") {
                    return Visibility::Crate;
                }
                return Visibility::Public;
            }
            "crate" => return Visibility::Crate,
            _ => {}
        }
    }
    Visibility::Private
}

fn extract_derives(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut derives = Vec::new();

    for item in node.children(&mut node.walk()) {
        if item.kind() == "attribute_item" {
            let text = ts_text(item, source);
            if let Some(inner) = text
                .strip_prefix("#[derive(")
                .and_then(|t| t.strip_suffix(")]"))
            {
                for d in inner.split(',') {
                    let trimmed = d.trim();
                    if !trimmed.is_empty() {
                        derives.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    if derives.is_empty() {
        if let Some(parent) = node.parent() {
            for i in 0..parent.child_count() {
                if let Some(sibling) = parent.child(i) {
                    if sibling.id() == node.id() {
                        break;
                    }
                    if sibling.kind() == "attribute_item" {
                        let text = ts_text(sibling, source);
                        if let Some(inner) = text
                            .strip_prefix("#[derive(")
                            .and_then(|t| t.strip_suffix(")]"))
                        {
                            for d in inner.split(',') {
                                let trimmed = d.trim();
                                if !trimmed.is_empty() {
                                    derives.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    derives
}

fn extract_doc_comment(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut prev = node.prev_sibling();
    let mut lines = Vec::new();

    while let Some(sibling) = prev {
        match sibling.kind() {
            "line_comment" | "block_comment" => {
                let text = ts_text(sibling, source);
                let stripped = text
                    .strip_prefix("///")
                    .or_else(|| text.strip_prefix("//!"));
                if let Some(doc) = stripped {
                    lines.push(doc.trim().to_string());
                }
                prev = sibling.prev_sibling();
            }
            _ => break,
        }
    }

    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function() {
        let source = "fn main() {\n    println!(\"hello\");\n}";
        let syms = RustExtractor.extract(source, Path::new("main.rs")).unwrap();
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name(), "main");
        assert_eq!(syms[0].kind_label(), "function");
    }

    #[test]
    fn parses_pub_function() {
        let source = "pub fn connect(addr: &str) -> Result<()> {\n    Ok(())\n}";
        let syms = RustExtractor.extract(source, Path::new("lib.rs")).unwrap();
        assert_eq!(syms[0].name(), "connect");
        assert_eq!(syms[0].visibility(), Visibility::Public);
    }

    #[test]
    fn parses_struct_with_derives() {
        let source = "#[derive(Debug, Clone)]\npub struct Config {\n    pub host: String,\n    port: u16,\n}";
        let syms = RustExtractor
            .extract(source, Path::new("config.rs"))
            .unwrap();
        assert_eq!(syms.len(), 1);
        let sym = &syms[0];
        assert_eq!(sym.name(), "Config");
        assert_eq!(sym.kind_label(), "struct");

        if let Symbol::Struct(def) = sym {
            assert!(def.derives.contains(&"Debug".to_string()));
            assert!(def.derives.contains(&"Clone".to_string()));
            assert_eq!(def.fields.len(), 2);
        }
    }

    #[test]
    fn parses_enum() {
        let source = "pub enum Status {\n    Active,\n    Inactive,\n}";
        let syms = RustExtractor
            .extract(source, Path::new("status.rs"))
            .unwrap();
        assert_eq!(syms[0].name(), "Status");
        assert_eq!(syms[0].kind_label(), "enum");
    }

    #[test]
    fn parses_trait() {
        let source = "pub trait Repository {\n    fn find(&self, id: u64) -> Option<Entity>;\n}";
        let syms = RustExtractor.extract(source, Path::new("repo.rs")).unwrap();
        assert_eq!(syms[0].name(), "Repository");
        assert_eq!(syms[0].kind_label(), "interface");
    }

    #[test]
    fn parses_mod_declaration() {
        let source = "pub mod database;";
        let syms = RustExtractor.extract(source, Path::new("lib.rs")).unwrap();
        assert_eq!(syms[0].name(), "database");
        assert_eq!(syms[0].kind_label(), "module");
    }
}
