use repoitdown_core::Pipeline;
use repoitdown_mcp::mcp_tool;
use repoitdown_mcp::protocol::{extract_id, read_message, write_error, write_message, write_response, JsonrpcError};
use repoitdown_mcp::ToolCallError;
use serde_json::json;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use tracing::{info, warn};

fn send_tools_list(writer: &mut dyn Write, id: &serde_json::Value) -> io::Result<()> {
    let tools_params = json!({ "tools": [mcp_tool()] });
    write_response(writer, id, Some(tools_params), None)
}

fn notify_tools_list_changed(writer: &mut dyn Write) -> io::Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": { "tools": [mcp_tool()] }
    });
    write_message(writer, &notification)
}

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(io::stderr)
        .try_init();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        let message = match read_message(&mut reader) {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(e) => {
                warn!("failed to read message from stdin: {e}");
                break;
            }
        };

        let request: serde_json::Value = match serde_json::from_str(&message) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to parse JSON-RPC message: {e}");
                if let Some(id) = extract_id(&message) {
                    write_error(
                        &mut writer,
                        &id,
                        JsonrpcError::new(-32700, "Parse error"),
                    ).ok();
                }
                continue;
            }
        };

        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = request.get("id").cloned();

        match method {
            "initialize" => {
                let result = json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "repoitdown-mcp",
                        "version": "0.1.0",
                    },
                    "capabilities": {
                        "tools": {
                            "listChanged": true
                        }
                    }
                });

                if let Some(ref req_id) = id {
                    write_response(&mut writer, req_id, Some(result), None).ok();
                }

                notify_tools_list_changed(&mut writer).ok();
            }

            "tools/list" => {
                if let Some(ref req_id) = id {
                    send_tools_list(&mut writer, req_id).ok();
                }
            }

            "tools/call" => {
                let full_params = request.get("params");
                let tool_name = full_params
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str());

                match tool_name {
                    None => {
                        warn!("tools/call missing required 'name' field");
                        if let Some(ref req_id) = id {
                            write_error(
                                &mut writer,
                                req_id,
                                JsonrpcError::new(
                                    -32602,
                                    "Missing required field: params.name",
                                ),
                            ).ok();
                        }
                        continue;
                    }
                    Some(name) if name != "get_codebase_topology" => {
                        warn!("unknown tool requested: {name}");
                        let tool_err = ToolCallError::method_not_found(name);
                        if let Some(ref req_id) = id {
                            write_error(
                                &mut writer,
                                req_id,
                                JsonrpcError::new(tool_err.code, tool_err.message),
                            ).ok();
                        }
                        continue;
                    }
                    _ => {}
                }

                let arguments = full_params.and_then(|p| p.get("arguments"));
                match handle_tool_call(arguments) {
                    Ok(result) => {
                        if let Some(ref req_id) = id {
                            write_response(&mut writer, req_id, Some(result), None).ok();
                        }
                    }
                    Err(err) => {
                        warn!("tool call error: {err}");
                        if let Some(ref req_id) = id {
                            write_error(
                                &mut writer,
                                req_id,
                                JsonrpcError::new(err.code, err.message),
                            ).ok();
                        }
                    }
                }
            }

            _ => {
                warn!("unknown method: {method}");
                if let Some(ref req_id) = id {
                    write_error(
                        &mut writer,
                        req_id,
                        JsonrpcError::new(-32601, format!("Method not found: {method}")),
                    ).ok();
                }
            }
        }
    }

    ExitCode::SUCCESS
}

fn handle_tool_call(params: Option<&serde_json::Value>) -> Result<serde_json::Value, ToolCallError> {
    let params = params.ok_or_else(|| ToolCallError::invalid_params("missing tool arguments"))?;

    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolCallError::invalid_params("missing required parameter: repo_path"))?
        .to_owned();
    let repo_path = std::path::PathBuf::from(repo_path);

    let mode_str = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("dump");

    let query = params
        .get("query")
        .and_then(|v| v.as_str());

    let max_tokens = params.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as usize);

    let mut pipeline = Pipeline::new();
    pipeline
        .configure(mode_str, query, max_tokens, true)
        .map_err(ToolCallError::invalid_params)?;

    info!("running pipeline on {} with mode {}", repo_path.display(), mode_str);

    match pipeline.run(&repo_path) {
        Ok(output) => {
            let result = json!({
                "content": [
                    {
                        "type": "text",
                        "text": output
                    }
                ]
            });
            Ok(result)
        }
        Err(e) => Err(ToolCallError::internal(format!("pipeline error: {e}"))),
    }
}
