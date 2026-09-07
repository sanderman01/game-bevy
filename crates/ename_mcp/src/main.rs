//! `ename_mcp` -- the MCP sidecar. Speaks MCP over stdio to a coding agent and forwards to the
//! game's BRP server over HTTP.
//!
//! Separate from the game on purpose. The engine gains one optional dependency rather than an
//! HTTP server, an MCP implementation and an async runtime; the agent's client launches this
//! itself so there is no port to allocate or client config to regenerate; and the catalogue
//! survives a game crash, so the agent can still read the logs after a panic.

mod brp;
mod entity;
mod server;
mod staleness;

use anyhow::Context as _;
use rmcp::{ServiceExt as _, transport::stdio};

/// Overrides where the game's BRP server is expected. Set it when the game runs with a
/// non-default `RemoteHttpPlugin` address.
const URL_VAR: &str = "ENAME_BRP_URL";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stderr, never stdout: stdout is the MCP transport and one stray line corrupts it.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Before serving, so the recorded build is the one this process actually started from.
    staleness::record();

    let url = std::env::var(URL_VAR).unwrap_or_else(|_| brp::DEFAULT_URL.to_owned());
    tracing::info!(%url, "forwarding to the game's remote server");

    let service = server::GameServer::new(brp::BrpClient::new(url))
        .serve(stdio())
        .await
        .context("failed to start the MCP server on stdio")?;

    service.waiting().await?;
    Ok(())
}
