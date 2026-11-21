//! Entry point: starts the HTTP server and the MCP (stdio) server in one
//! process and shuts both down gracefully on SIGINT/SIGTERM.

mod http_server;
mod mcp_server;
mod search;

use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

const DEFAULT_ADDR: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 3333;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct Config {
    http: bool,
    mcp: bool,
    host: String,
    port: u16,
}

impl Config {
    /// Flags win over environment variables:
    /// `--http-only`, `--mcp-only`, `WEBSEARCH_HOST`, `WEBSEARCH_PORT` / `PORT`.
    fn from_env() -> Result<Self> {
        let mut http = true;
        let mut mcp = true;

        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--http-only" => mcp = false,
                "--mcp-only" => http = false,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => {
                    anyhow::bail!("unknown argument: {other} (see --help)");
                }
            }
        }

        if !http && !mcp {
            anyhow::bail!("both servers are disabled");
        }

        let host = std::env::var("WEBSEARCH_HOST").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
        let port_raw = std::env::var("WEBSEARCH_PORT")
            .or_else(|_| std::env::var("PORT"))
            .unwrap_or_else(|_| DEFAULT_PORT.to_string());
        let port = port_raw
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid port: {port_raw:?}"))?;

        Ok(Self {
            http,
            mcp,
            host,
            port,
        })
    }
}

fn print_usage() {
    println!(
        "sh_web_search_tool_rs {} — DuckDuckGo search over HTTP and MCP

USAGE
  {bin} [flags]

FLAGS
  --http-only    run only the HTTP server (systemd / HTTP backend deployments)
  --mcp-only     run only the MCP stdio server (MCP client spawns the binary)
  -h, --help     show this help

ENVIRONMENT
  WEBSEARCH_HOST   HTTP bind address (default: 0.0.0.0)
  WEBSEARCH_PORT   HTTP port (default: 3333, `PORT` is also accepted)
  RUST_LOG         log filter (default: info)
",
        env!("CARGO_PKG_VERSION"),
        bin = std::env::args()
            .next()
            .unwrap_or_else(|| "web-search".to_string()),
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Config::from_env()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        http = config.http,
        mcp = config.mcp,
        addr = %format!("{}:{}", config.host, config.port),
        "starting web search service"
    );

    // One token drives both servers: cancelling it stops HTTP (axum
    // graceful shutdown) and MCP (serve_with_ct / RunningService::cancel).
    let shutdown = CancellationToken::new();
    let http_ct = shutdown.child_token();
    let mcp_ct = shutdown.child_token();

    let http_enabled = config.http;
    let mcp_enabled = config.mcp;
    let host = config.host.clone();
    let port = config.port;

    let http_task = tokio::spawn(async move {
        if !http_enabled {
            return Ok(());
        }
        http_server::serve(&host, port, http_ct).await
    });
    let mcp_task = tokio::spawn(async move {
        if !mcp_enabled {
            return Ok(());
        }
        mcp_server::serve(mcp_ct).await
    });

    // Both servers run concurrently. Whichever side ends first (an MCP client
    // disconnecting, an HTTP shutdown) releases the join; a signal cancels
    // both with a grace period.
    let servers = async move {
        let http = match http_task.await {
            Ok(res) => res,
            Err(join_err) => Err(anyhow::Error::from(join_err)),
        };
        let mcp = match mcp_task.await {
            Ok(res) => res,
            Err(join_err) => Err(anyhow::Error::from(join_err)),
        };
        (http, mcp)
    };
    tokio::pin!(servers);

    let (http_result, mcp_result) = tokio::select! {
        outcome = &mut servers => outcome,
        _ = wait_for_shutdown_signal() => {
            tracing::info!("shutdown signal received, stopping servers");
            shutdown.cancel();
            tokio::time::timeout(SHUTDOWN_GRACE, &mut servers)
                .await
                .unwrap_or_else(|_| {
                    tracing::warn!("servers did not stop within {SHUTDOWN_GRACE:?}, exiting");
                    (Ok(()), Ok(()))
                })
        }
    };

    // A stdio MCP client is the more important peer when both are enabled:
    // a port conflict must not take the whole process down.
    match (http_result, mcp_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(err)) => Err(err),
        (Err(err), Ok(())) if mcp_enabled => {
            tracing::warn!("HTTP server unavailable: {err:#} (MCP keeps running)");
            Ok(())
        }
        (Err(http_err), Err(mcp_err)) => {
            tracing::error!("MCP server failed: {mcp_err:#}");
            Err(http_err)
        }
        (Err(err), Ok(())) => Err(err),
    }
}

/// Logs must go to stderr: stdout carries the MCP JSON-RPC stream.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        let mut interrupt =
            signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received"),
            _ = interrupt.recv() => tracing::info!("SIGINT received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
