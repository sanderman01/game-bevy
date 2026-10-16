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
//! | `ename_mcp game <start\|stop\|restart\|status\|owner> [flags]` | The launcher's CLI, aliased as `cargo game`. |
//! | `ename_mcp supervise <dir> <generation>` | The detached supervisor of one game run. Started by the launcher, never by hand. |

mod brp;
mod entity;
mod launch;
mod server;
mod staleness;

use std::process::ExitCode;

use anyhow::Context as _;
use rmcp::{ServiceExt as _, transport::stdio};

/// Overrides where the game's BRP server is expected, and pins it: the session then never
/// follows a started instance to another port.
const URL_VAR: &str = "ENAME_BRP_URL";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let role = args.first().map(String::as_str);
    // stderr, never stdout: stdout is the MCP transport and one stray line corrupts it. The
    // supervisor's stderr is a log file, so no colour codes there; the CLI's output is its
    // summary line, so only warnings.
    let default_level = if role == Some("game") { "warn" } else { "info" };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(role != Some("supervise"))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .init();
    launch::record_exe();

    let result = match role {
        None => serve().await,
        Some("game") => return launch::cli::run(&args[1..]).await,
        Some("supervise") => launch::supervise::run(&args[1..]).await,
        Some(other) => Err(anyhow::anyhow!(
            "unknown command `{other}`; `ename_mcp game --help` lists the launcher's"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
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

    let lifecycle = project
        .clone()
        .map(|project| server::Lifecycle::new(launch::Launcher::new(project), owner.clone()));
    let endpoint = brp::Endpoint::new(pinned, project, owner);
    let service = server::GameServer::with_endpoint(endpoint, lifecycle)
        .serve(stdio())
        .await
        .context("failed to start the MCP server on stdio")?;

    service.waiting().await?;
    Ok(())
}
