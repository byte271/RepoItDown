//! RepoItDown REST API server.
//!
//! A stateless HTTP server that wraps `repoitdown-core::Pipeline` behind a
//! single POST endpoint. Each request runs one full pipeline invocation.
//!
//! ## Endpoints
//!
//! - `GET  /health`              — liveness probe
//! - `POST /api/v1/topology`     — run the pipeline and return Markdown topology
//!
//! ## Usage
//!
//! ```bash
//! repoitdown-server --port 8080
//! curl -X POST http://localhost:8080/api/v1/topology \
//!   -H "Content-Type: application/json" \
//!   -d '{"repo_path": ".", "mode": "architect", "max_tokens": 8000}'
//! ```

use axum::{extract::State, http::StatusCode, routing, Json, Router};
use repoitdown_core::Pipeline;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::info;

/// Request body for `POST /api/v1/topology`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct TopologyRequest {
    pub repo_path: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub no_collapse: bool,
}

fn default_mode() -> String {
    "dump".to_string()
}

/// Response body for `POST /api/v1/topology`.
#[derive(Debug, Serialize)]
struct TopologyResponse {
    pub output: String,
    pub files: usize,
    pub tokens: usize,
}

/// Standard error response body.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ----------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

async fn topology(
    State(()): State<()>,
    Json(req): Json<TopologyRequest>,
) -> Result<Json<TopologyResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut pipeline = Pipeline::new();
    pipeline
        .configure(&req.mode, req.query.as_deref(), req.max_tokens, !req.no_collapse)
        .map_err(|msg| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "invalid_params".into(),
                    detail: Some(msg.to_string()),
                }),
            )
        })?;

    let repo_path = PathBuf::from(&req.repo_path);
    info!("running pipeline on {} with mode {}", repo_path.display(), req.mode);

    let output = tokio::task::spawn_blocking(move || pipeline.run(&repo_path))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "internal_error".into(),
                    detail: Some(format!("pipeline join error: {e}")),
                }),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "pipeline_error".into(),
                    detail: Some(e.to_string()),
                }),
            )
        })?;

    let files = output.matches("\n```").count();
    let tokens = repoitdown_core::count_tokens(&output).unwrap_or_else(|_| {
        (output.split_whitespace().count() as f64 * 1.3) as usize
    });

    Ok(Json(TopologyResponse {
        output,
        files,
        tokens,
    }))
}

// ----------------------------------------------------------------
// Main
// ----------------------------------------------------------------

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .try_init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let app = Router::new()
        .route("/health", routing::get(health))
        .route("/api/v1/topology", routing::post(topology))
        .with_state(());

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    info!("RepoItDown REST API server listening on http://{addr}");

    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
            eprintln!("failed to bind to {addr}: {e}");
            std::process::exit(1);
        });
        axum::serve(listener, app).await.unwrap_or_else(|e| {
            eprintln!("server error: {e}");
            std::process::exit(1);
        });
    });

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/health", routing::get(health))
            .route("/api/v1/topology", routing::post(topology))
            .with_state(())
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn topology_missing_repo_path_returns_client_error() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"mode": "dump"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }

    #[tokio::test]
    async fn topology_unknown_mode_returns_400() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"repo_path": ".", "mode": "invalid_mode"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn topology_max_tokens_zero_returns_400() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"repo_path": ".", "max_tokens": 0}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn topology_dump_mode_succeeds() {
        let tmp = std::env::temp_dir().join("repoitdown_server_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("main.rs"), "fn main() {}\n").unwrap();

        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/topology")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"repo_path": "{}", "mode": "dump"}}"#,
                        tmp.display().to_string().replace('\\', "\\\\")
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();

        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["output"].as_str().unwrap().contains("main.rs"));
        assert!(parsed["files"].as_u64().unwrap() >= 1);
    }
}
