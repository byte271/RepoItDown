use tree_sitter::Node;

use crate::types::SourceLocation;

#[must_use]
pub fn ts_pos(node: Node<'_>) -> SourceLocation {
    let start = node.start_position();
    let end = node.end_position();
    SourceLocation {
        file: std::path::PathBuf::new(),
        line_start: start.row + 1,
        line_end: end.row + 1,
        col_start: start.column + 1,
        col_end: end.column + 1,
    }
}

#[must_use]
pub fn ts_text<'src>(node: Node<'_>, source: &'src str) -> &'src str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

#[must_use]
pub fn ts_loc(node: Node<'_>, path: &std::path::Path) -> SourceLocation {
    let mut loc = ts_pos(node);
    loc.file = path.to_path_buf();
    loc
}

/// Pre-order walks every node in the tree, starting at `node`.
///
/// The visitor returns `true` to descend into a node's children and `false` to
/// prune that subtree. Pruning is what keeps import and call collection from
/// re-entering ranges that have already been accounted for.
pub fn visit_nodes<F>(node: Node<'_>, visitor: &mut F)
where
    F: FnMut(Node<'_>) -> bool,
{
    if !visitor(node) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_nodes(child, visitor);
    }
}

/// Strips the surrounding quote characters from a string literal's text.
#[must_use]
pub fn strip_quotes(text: &str) -> &str {
    text.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

/// Walks up the tree-sitter ancestor chain from `node` to find the nearest
/// enclosing function-like declaration, then returns its name.
///
/// `function_kinds` is the set of tree-sitter node kinds that represent
/// function or method declarations in the target language (e.g.
/// `["function_item"]` for Rust, `["function_definition"]` for Python).
///
/// Returns `None` if the call is at module level (not inside any function).
#[must_use]
pub fn enclosing_function_name(
    node: Node<'_>,
    source: &str,
    function_kinds: &[&str],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if function_kinds.contains(&parent.kind()) {
            if let Some(name_node) = parent.child_by_field_name("name") {
                let name = ts_text(name_node, source);
                if !name.is_empty() {
                    return Some(name.to_owned());
                }
            }
            // Found a function-like node but couldn't read its name —
            // stop searching to avoid returning a grandparent's name.
            return None;
        }
        current = parent.parent();
    }
    None
}

pub fn parse_source(
    language: &tree_sitter::Language,
    source: &str,
    path: &std::path::Path,
) -> crate::error::Result<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .map_err(|e| crate::error::Error::Parse {
            path: path.to_path_buf(),
            message: format!("failed to set grammar: {e}"),
        })?;
    parser.parse(source, None).ok_or_else(|| {
        crate::error::Error::Parse {
            path: path.to_path_buf(),
            message: "tree-sitter returned null tree".into(),
        }
    })
}
