//! MCP (Model Context Protocol) transport over stdio.
//!
//! Exposes the shared search core ([`crate::search`]) as the `web_search`
//! tool so that any MCP client (OpenCode, Claude Desktop, Cherry Studio, …)
//! can call it.

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::search::{DEFAULT_MAX_RESULTS, render_markdown, web_search};

/// MCP server handle: stateless, the search client pool lives in `search`.
#[derive(Clone, Default)]
pub struct WebSearchServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WebSearchParams {
    /// The search query.
    #[schemars(description = "Search query")]
    pub query: String,
    /// How many results to return (default: 3).
    #[serde(default = "default_max_results")]
    #[schemars(description = "Maximum number of results to return (default: 3)")]
    pub max_results: usize,
    /// Whether to download and extract the content of each page (default: true).
    #[serde(default = "default_true")]
    #[schemars(description = "Fetch full page content for every hit (default: true)")]
    pub fetch_content: bool,
}

fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

fn default_true() -> bool {
    true
}

#[tool_router]
impl WebSearchServer {
    /// Search the web via DuckDuckGo and return Markdown for an LLM.
    #[tool(
        name = "web_search",
        description = "Search the web (DuckDuckGo) and return the found pages with title, URL, snippet and extracted content as Markdown"
    )]
    async fn web_search(
        &self,
        Parameters(params): Parameters<WebSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.query.trim().is_empty() {
            return Err(McpError::invalid_request("query must not be empty", None));
        }
        match web_search(&params.query, params.max_results, params.fetch_content).await {
            Ok(results) if results.is_empty() => {
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "No results found for '{}'",
                    params.query.trim()
                ))]))
            }
            Ok(results) => Ok(CallToolResult::success(vec![ContentBlock::text(
                render_markdown(params.query.trim(), &results),
            )])),
            Err(err) => {
                tracing::error!("MCP web_search failed: {err:#}");
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Search failed: {err}"
                ))]))
            }
        }
    }
}

#[tool_handler(
    name = "web-search",
    version = "0.2.0",
    instructions = "Use web_search to look up current information. Parameters: query (required), max_results (default 3), fetch_content (default true)."
)]
impl ServerHandler for WebSearchServer {}

/// Serve MCP on stdin/stdout until `ct` is cancelled or the client disconnects.
pub async fn serve(ct: CancellationToken) -> anyhow::Result<()> {
    tracing::info!("MCP server (stdio) waiting for a client handshake");
    let service = WebSearchServer.serve_with_ct(stdio(), ct).await?;
    tracing::info!("MCP server started");
    let reason = service.waiting().await?;
    tracing::info!("MCP server stopped: {reason:?}");
    Ok(())
}
