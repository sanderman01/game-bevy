//! `ename_mcp` -- the MCP sidecar. Speaks MCP over stdio to a coding agent and forwards to the
//! game's BRP server over HTTP.
//!
//! Separate from the game on purpose. The engine gains one optional dependency rather than an
//! HTTP server, an MCP implementation and an async runtime; the agent's client launches this
//! itself so there is no port to allocate or client config to regenerate; and the catalogue
//! survives a game crash, so the agent can still read the logs after a panic.
//!
//! The same binary is the game launcher (see [`launch`]). Its role comes from argv:
//!
//! | Invocation | Role |
//! | --- | --- |
//! | `ename_mcp` | The MCP server. |
//! | `ename_mcp supervise <dir> <generation>` | The detached supervisor of one game run. Started by the launcher, never by hand. |

mod brp;
mod entity;
#[allow(
    dead_code,
    unused_imports,
    reason = "the MCP tools and the CLI that use it land in the next commits"
)]
mod launch;
mod server;
mod staleness;

use anyhow::Context as _;
use rmcp::{ServiceExt as _, transport::stdio};

/// Overrides where the game's BRP server is expected, and pins it: the session then never
/// follows a started instance to another port.
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
    launch::record_exe();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => serve().await,
        Some("supervise") => launch::supervise::run(&args[1..]).await,
        Some(other) => anyhow::bail!("unknown command `{other}`"),
    }
}

async fn serve() -> anyhow::Result<()> {
    // Before serving, so the recorded build is the one this process actually started from.
    staleness::record();

    let pinned = std::env::var(URL_VAR).ok();
    let cwd = std::env::current_dir().context("no working directory")?;
    let project = match launch::Project::discover(&cwd).await {
        Ok(project) => Some(project),
        Err(error) => {
            tracing::warn!("no game launcher for this session: {error:#}");
            None
        }
    };
    let owner = launch::mcp_owner();
    tracing::info!(?pinned, ?owner, "forwarding to the game's remote server");

    let endpoint = brp::Endpoint::new(pinned, project, owner);
    let service = server::GameServer::with_endpoint(endpoint)
        .serve(stdio())
        .await
        .context("failed to start the MCP server on stdio")?;

    service.waiting().await?;
    Ok(())
}
