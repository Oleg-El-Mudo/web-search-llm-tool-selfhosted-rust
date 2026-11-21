//! HTTP transport: Axum routers and request handlers.
//!
//! Thin layer on top of [`crate::search`] — all search logic lives there.

use std::net::{SocketAddr, ToSocketAddrs};

use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::search::{DEFAULT_MAX_RESULTS, SearchResult, web_search};

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub fetch_content: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

pub fn router() -> Router {
    Router::new()
        // POST /search with a JSON body, plus GET /search?query=... for manual testing.
        .route("/search", post(handle_search_post).get(handle_search_get))
        .route("/health", get(handle_health))
}

async fn handle_search_post(
    Json(payload): Json<SearchRequest>,
) -> Result<Json<Vec<SearchResult>>, (StatusCode, Json<ErrorResponse>)> {
    let results = web_search(&payload.query, payload.max_results, payload.fetch_content)
        .await
        .map_err(search_error_response)?;
    Ok(Json(results))
}

async fn handle_health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

fn search_error_response(err: crate::search::SearchError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match &err {
        crate::search::SearchError::Blocked => StatusCode::BAD_GATEWAY,
        crate::search::SearchError::Http(_) => StatusCode::BAD_GATEWAY,
    };
    tracing::error!("Search failed: {err:#}");
    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

/// Bind `host:port` and serve HTTP until `ct` is cancelled.
pub async fn serve(host: &str, port: u16, ct: CancellationToken) -> anyhow::Result<()> {
    let addr = resolve_addr(host, port).await?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HTTP server listening on http://{addr}");

    axum::serve(listener, router())
        .with_graceful_shutdown(async move {
            ct.cancelled().await;
        })
        .await?;

    tracing::info!("HTTP server stopped");
    Ok(())
}

/// Accept a JSON search request via GET as well (`/search?query=...`),
/// which is handy for manual testing with curl.
#[allow(dead_code)]
async fn handle_search_get(
    Query(payload): Query<SearchRequest>,
) -> Result<Json<Vec<SearchResult>>, (StatusCode, Json<ErrorResponse>)> {
    handle_search_post(Json(payload)).await
}

async fn resolve_addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let target = format!("{host}:{port}");
    let addr = tokio::task::spawn_blocking(move || target.to_socket_addrs())
        .await??
        .next()
        .ok_or_else(|| anyhow::anyhow!("could not resolve {host}:{port}"))?;
    Ok(addr)
}
