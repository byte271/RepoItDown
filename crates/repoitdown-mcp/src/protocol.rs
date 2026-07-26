use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

/// Maximum allowed Content-Length for incoming messages (50 MiB).
/// Rejects oversized messages to prevent memory exhaustion attacks.
pub const MAX_CONTENT_LENGTH: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonrpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonrpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonrpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl JsonrpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

pub fn read_message(reader: &mut dyn BufRead) -> io::Result<Option<String>> {
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        headers.push_str(&line);
    }

    if headers.is_empty() {
        return Ok(None);
    }

    let content_length = headers
        .lines()
        .find(|l| l.to_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    if content_length == 0 {
        return Ok(None);
    }

    if content_length > MAX_CONTENT_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Content-Length {content_length} exceeds maximum {MAX_CONTENT_LENGTH}"),
        ));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).to_string()))
}

pub fn write_message(writer: &mut dyn Write, value: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_string(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()?;
    Ok(())
}

pub fn write_response(
    writer: &mut dyn Write,
    id: &serde_json::Value,
    result: Option<serde_json::Value>,
    error: Option<JsonrpcError>,
) -> io::Result<()> {
    let response = JsonrpcResponse {
        jsonrpc: "2.0".to_string(),
        id: id.clone(),
        result,
        error,
    };
    // Serialize directly to string to avoid the double-serialization
    // overhead of serde_json::to_value → serde_json::to_string.
    let body = serde_json::to_string(&response)?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()?;
    Ok(())
}

pub fn write_error(
    writer: &mut dyn Write,
    id: &serde_json::Value,
    error: JsonrpcError,
) -> io::Result<()> {
    write_response(writer, id, None, Some(error))
}

pub fn extract_id(raw: &str) -> Option<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    parsed.get("id").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----------------------------------------------------------------
    // read_message
    // ----------------------------------------------------------------

    #[test]
    fn read_message_parses_valid_frame() {
        let body = r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = io::BufReader::new(frame.as_bytes());
        let result = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_handles_lowercase_header() {
        let body = r#"{"jsonrpc":"2.0"}"#;
        let frame = format!("content-length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = io::BufReader::new(frame.as_bytes());
        let result = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_returns_none_on_empty_headers() {
        let frame = "\r\n";
        let mut reader = io::BufReader::new(frame.as_bytes());
        let result = read_message(&mut reader).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_message_rejects_oversized_length() {
        let oversized = MAX_CONTENT_LENGTH + 1;
        let frame = format!("Content-Length: {}\r\n\r\n", oversized);
        let mut reader = io::BufReader::new(frame.as_bytes());
        let result = read_message(&mut reader);
        assert!(result.is_err());
    }

    #[test]
    fn read_message_allows_exact_max_length() {
        // Content-Length equal to MAX_CONTENT_LENGTH should be accepted.
        let frame = format!("Content-Length: {}\r\n\r\n", MAX_CONTENT_LENGTH);
        let mut reader = io::BufReader::new(frame.as_bytes());
        // We expect an unexpected EOF since there's no actual body,
        // but NOT an InvalidData error.
        let result = read_message(&mut reader);
        assert!(result.is_err());
        // Should be an unexpected EOF, not InvalidData.
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    // ----------------------------------------------------------------
    // write_message
    // ----------------------------------------------------------------

    #[test]
    fn write_message_produces_valid_frame() {
        let value = serde_json::json!({"key": "value"});
        let mut buf = Vec::new();
        write_message(&mut buf, &value).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.starts_with("Content-Length: "));
        assert!(output.contains("\r\n\r\n"));
        assert!(output.contains(r#""key":"value""#));
    }

    // ----------------------------------------------------------------
    // write_response
    // ----------------------------------------------------------------

    #[test]
    fn write_response_includes_id_and_result() {
        let id = serde_json::json!(42);
        let result = serde_json::json!({"ok": true});
        let mut buf = Vec::new();
        write_response(&mut buf, &id, Some(result), None).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains(r#""jsonrpc":"2.0""#));
        assert!(output.contains(r#""id":42"#));
        assert!(output.contains(r#""ok":true"#));
        // Error field should be absent on success.
        assert!(!output.contains(r#""error""#));
    }

    #[test]
    fn write_response_includes_error_and_no_result() {
        let id = serde_json::json!(1);
        let err = JsonrpcError::new(-32600, "invalid request");
        let mut buf = Vec::new();
        write_response(&mut buf, &id, None, Some(err)).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains(r#""error""#));
        assert!(output.contains(r#""code":-32600"#));
        assert!(output.contains(r#""invalid request""#));
        // Result field should be absent on error.
        assert!(!output.contains(r#""result""#));
    }

    // ----------------------------------------------------------------
    // write_error
    // ----------------------------------------------------------------

    #[test]
    fn write_error_is_convenience_alias() {
        let id = serde_json::json!("req-1");
        let err = JsonrpcError::new(-32000, "server error");
        let mut buf = Vec::new();
        write_error(&mut buf, &id, err).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains(r#""error""#));
        assert!(output.contains(r#""id":"req-1""#));
        assert!(!output.contains(r#""result""#));
    }

    // ----------------------------------------------------------------
    // extract_id
    // ----------------------------------------------------------------

    #[test]
    fn extract_id_from_valid_json() {
        let raw = r#"{"jsonrpc":"2.0","id":7,"method":"test"}"#;
        let id = extract_id(raw).unwrap();
        assert_eq!(id, serde_json::json!(7));
    }

    #[test]
    fn extract_id_returns_none_on_invalid_json() {
        assert!(extract_id("not json").is_none());
    }

    #[test]
    fn extract_id_returns_none_when_missing() {
        let raw = r#"{"jsonrpc":"2.0","method":"notification"}"#;
        // JSON-RPC notifications have no id field; extract_id returns None.
        assert!(extract_id(raw).is_none());
    }

    // ----------------------------------------------------------------
    // JsonrpcError
    // ----------------------------------------------------------------

    #[test]
    fn jsonrpc_error_new_sets_code_and_message() {
        let err = JsonrpcError::new(-32700, "Parse error");
        assert_eq!(err.code, -32700);
        assert_eq!(err.message, "Parse error");
        assert!(err.data.is_none());
    }
}
