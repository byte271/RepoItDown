use std::collections::BTreeMap;
use std::fmt::Write;

use crate::types::{FileNode, Symbol};

#[derive(Clone)]
pub struct RenderConfig {
    pub collapse: bool,
    pub contract_view: bool,
    pub max_tokens: Option<usize>,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            collapse: true,
            contract_view: true,
            max_tokens: None,
        }
    }
}

#[must_use]
pub fn render(nodes: &[FileNode], config: &RenderConfig) -> String {
    let total_tokens: usize = nodes.iter().map(|n| n.token_count).sum();
    let total_symbols: usize = nodes.iter().map(|n| n.symbols.len()).sum();
    let total_source: usize = nodes.iter().map(|n| n.source.len()).sum();
    let mut out = String::with_capacity(total_source + total_source / 3);

    writeln!(out, "# RepoItDown — Repository Topology\n").unwrap();
    writeln!(
        out,
        "**{files} files · {tokens} tokens · {symbols} symbols**\n",
        files = nodes.len(),
        tokens = total_tokens,
        symbols = total_symbols
    )
    .unwrap();

    render_summary_table(nodes, total_tokens, &mut out);

    if config.contract_view && total_symbols > 0 {
        render_contract_view(nodes, &mut out);
    }

    writeln!(out, "## Source Files\n").unwrap();

    for node in nodes {
        render_file_node(node, config, &mut out);
    }

    if let Some(limit) = config.max_tokens {
        if total_tokens > limit {
            writeln!(
                out,
                "> ⚠️ **Warning**: output ({total_tokens}) exceeds max-tokens limit ({limit})\n"
            )
            .unwrap();
        }
    }

    out
}

fn render_summary_table(nodes: &[FileNode], total_tokens: usize, out: &mut String) {
    let mut dirs: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for node in nodes {
        let dir = node.directory().to_string_lossy().into_owned();
        let entry = dirs.entry(dir).or_default();
        entry.0 += 1;
        entry.1 += node.token_count;
    }

    writeln!(out, "## Structural Summary\n").unwrap();
    writeln!(out, "| Directory | Files | Tokens |").unwrap();
    writeln!(out, "|-----------|-------|--------|").unwrap();

    for (dir, (files, tokens)) in &dirs {
        writeln!(out, "| `{dir}` | {files} | {tokens} |").unwrap();
    }

    writeln!(
        out,
        "| **Total** | **{total_files}** | **{total_tokens}** |\n",
        total_files = nodes.len(),
        total_tokens = total_tokens,
    )
    .unwrap();
}

fn render_contract_view(nodes: &[FileNode], out: &mut String) {
    writeln!(out, "## Contract View\n").unwrap();

    for node in nodes {
        let exported: Vec<&Symbol> = node
            .symbols
            .iter()
            .filter(|s| s.visibility().is_exported())
            .collect();

        if exported.is_empty() {
            continue;
        }

        writeln!(out, "### `{}`\n", node.path.display()).unwrap();

        for sym in &exported {
            let doc_suffix = sym
                .docstring()
                .map_or(String::new(), |d| format!(" — {d}"));
            writeln!(
                out,
                "- `{kind}` **{name}**{doc_suffix}",
                kind = sym.kind_label(),
                name = sym.name(),
                doc_suffix = doc_suffix,
            )
            .unwrap();
        }
        writeln!(out).unwrap();
    }
}

fn render_file_node(node: &FileNode, config: &RenderConfig, out: &mut String) {
    let path_display = node.path.display();
    let lang_display = node.language.to_string();
    let redacted = if node.has_redactions {
        " ⚠️ redacted"
    } else {
        ""
    };

    let fence_tag = node
        .language
        .canonical_extension()
        .or_else(|| node.path.extension().and_then(|e| e.to_str()))
        .unwrap_or("text");

    if config.collapse {
        writeln!(
            out,
            "<details>\n<summary><code>{path}</code> <em>{lang} · {tokens} tokens{redacted}</em></summary>\n",
            path = html_escape(&path_display.to_string()),
            lang = lang_display,
            tokens = node.token_count,
            redacted = redacted,
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "### `{path}` <em>{lang} · {tokens} tokens{redacted}</em>\n",
            path = path_display,
            lang = lang_display,
            tokens = node.token_count,
            redacted = redacted,
        )
        .unwrap();
    }

    writeln!(out, "```{fence_tag}").unwrap();
    out.push_str(&node.source);
    if !node.source.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out, "```").unwrap();

    if !node.symbols.is_empty() {
        writeln!(out, "\n**Extracted Symbols:**\n").unwrap();
        for sym in &node.symbols {
            let loc = sym.location();
            writeln!(
                out,
                "- L{line}: `{kind}` **{name}**",
                line = loc.line_start,
                kind = sym.kind_label(),
                name = sym.name(),
            )
            .unwrap();
        }
    }

    if config.collapse {
        writeln!(out, "\n</details>\n").unwrap();
    } else {
        writeln!(out).unwrap();
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileNode, FunctionDef, Language, SourceLocation, Symbol, Visibility};

    fn make_node(path: &str, source: &str) -> FileNode {
        FileNode::new(
            std::path::PathBuf::from(path),
            Language::Rust,
            source.to_string(),
        )
    }

    fn make_fn_node() -> FileNode {
        let loc = SourceLocation::line_only(std::path::PathBuf::from("src/main.rs"), 1);
        let func = FunctionDef {
            name: "main".into(),
            visibility: Visibility::Public,
            signature: "fn main()".into(),
            docstring: Some("Entry point".into()),
            parameters: vec![],
            return_type: None,
            body_stripped: false,
            loc,
        };
        let mut node = make_node("src/main.rs", "fn main() {\n    println!(\"hello\");\n}");
        node.symbols = vec![Symbol::from(func)];
        node.token_count = 15;
        node
    }

    #[test]
    fn renders_single_file() {
        let node = make_node("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }");
        let output = render(&[node], &RenderConfig::default());
        assert!(output.contains("src/lib.rs"));
        assert!(output.contains("```rs"));
        assert!(output.contains("pub fn add"));
    }

    #[test]
    fn renders_with_contract_view() {
        let node = make_fn_node();
        let output = render(&[node], &RenderConfig::default());
        assert!(output.contains("Contract View"));
        assert!(output.contains("**main**"));
    }

    #[test]
    fn renders_without_collapse() {
        let node = make_node("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }");
        let config = RenderConfig {
            collapse: false,
            ..RenderConfig::default()
        };
        let output = render(&[node], &config);
        assert!(!output.contains("<details>"));
        assert!(output.contains("### `src/lib.rs`"));
    }

    #[test]
    fn renders_summary_table() {
        let a = make_node("src/main.rs", "fn main() {}");
        let b = make_node("tests/test.rs", "#[test]\nfn test() {}");
        let output = render(&[a, b], &RenderConfig::default());
        assert!(output.contains("Structural Summary"));
        assert!(output.contains("`src`"));
        assert!(output.contains("`tests`"));
    }

    #[test]
    fn warns_on_token_budget_exceeded() {
        let node = make_fn_node();
        let config = RenderConfig {
            max_tokens: Some(5),
            ..RenderConfig::default()
        };
        let output = render(&[node], &config);
        assert!(output.contains("exceeds max-tokens limit"));
    }

    #[test]
    fn html_escapes_file_paths() {
        let node = make_node("src/<evil>.rs", "fn main() {}");
        let output = render(&[node], &RenderConfig::default());
        assert!(output.contains("&lt;evil&gt;"));
        assert!(!output.contains("<evil>"));
    }

    #[test]
    fn marks_redacted_files() {
        let mut node = make_node("src/secrets.rs", "const KEY = \"[REDACTED]\";");
        node.has_redactions = true;
        let output = render(&[node], &RenderConfig::default());
        assert!(output.contains("redacted"));
    }
}
