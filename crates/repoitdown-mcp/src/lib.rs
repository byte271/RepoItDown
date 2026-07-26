use serde::{Deserialize, Serialize};

pub mod protocol;

/// Errors that can occur during tool call handling, mapped to JSON-RPC
/// error codes for proper client-side error attribution.
#[derive(Debug, Clone)]
pub struct ToolCallError {
    /// JSON-RPC error code.
    pub code: i64,
    /// Human-readable error message.
    pub message: String,
}

impl ToolCallError {
    /// Invalid params — the client provided missing or malformed arguments.
    /// JSON-RPC code: -32602.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    /// Method not found — the requested tool does not exist.
    /// JSON-RPC code: -32601.
    pub fn method_not_found(tool_name: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: format!("Tool not found: {}", tool_name.into()),
        }
    }

    /// Internal error — something went wrong during tool execution.
    /// JSON-RPC code: -32603.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ToolCallError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

pub fn mcp_tool() -> McpTool {
    McpTool {
        name: "get_codebase_topology",
        description: "Transform a repository into a token-optimized Markdown topology for LLM context windows. Uses AST parsing, dependency graphs, and adaptive slicing to maximize LLM reasoning accuracy per token.",
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "repo_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the repository root"
                },
                "mode": {
                    "type": "string",
                    "enum": ["dump", "explore", "architect", "task"],
                    "default": "dump",
                    "description": "Processing mode: dump (full source), explore (full source + contract view), architect (skeletonized with PageRank hubs), task (BM25 query + k-hop slicing)"
                },
                "query": {
                    "type": "string",
                    "description": "Natural-language query for task mode. Required when mode is 'task'."
                },
                "max_tokens": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum output tokens. Serves as slicing budget for architect and task modes."
                }
            },
            "required": ["repo_path"]
        }),
    }
}
