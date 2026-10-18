//! `game_start`, `game_stop`, `game_restart` and `game_status`: the launcher as MCP tools.
//!
//! They work with the game down, so they do not go through [`GameServer::tag`]. A launch that
//! ends in `build_failed`, `port_in_use` or a crash, and a refused stop, is a tool error whose
//! text carries the errors, holder or crash summary (rule 4: a client may drop an error's data).
//! `game_status` reports any state as a normal result, since reporting is its job.

use std::time::Duration;

use rmcp::{ErrorData, Json, handler::server::wrapper::Parameters, schemars, tool, tool_router};
use serde::Deserialize;

use super::{GameServer, commands::MAX_BLOCK, fail};
use crate::{
    launch::{Caller, DEFAULT_INSTANCE, GameState, LaunchRequest, LaunchUpdate, Launcher, Status},
    staleness,
};

/// How a session reaches the launcher: the workspace's, and who this session is.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    pub launcher: Launcher,
    pub caller: Caller,
}

impl Lifecycle {
    pub fn new(launcher: Launcher, owner: Option<String>) -> Self {
        Self {
            launcher,
            caller: Caller {
                owner,
                follow_hint: "call game_status with wait_seconds (up to 50) to follow it",
            },
        }
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct StartParams {
    /// Which launch. Defaults to "default" (port 15702); name another to run a second game beside
    /// it on its own ports.
    #[serde(default)]
    pub instance: Option<String>,
    /// Pin the BRP port; the render world listens on the next one.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(flatten)]
    pub launch: LaunchUpdate,
    /// How long to wait for ready, 0 to 50 seconds. Defaults to 50.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct RestartParams {
    #[serde(flatten)]
    pub start: StartParams,
    /// Restart a game someone else started. Only when the user asked you to.
    #[serde(default)]
    pub take_over: bool,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct StopParams {
    /// Defaults to "default".
    #[serde(default)]
    pub instance: Option<String>,
    /// Stop a game someone else started. Only when the user asked you to.
    #[serde(default)]
    pub take_over: bool,
    /// How long the game gets to exit by itself before it is killed. Defaults to 5.
    #[serde(default)]
    pub grace_seconds: Option<f64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct StatusParams {
    /// Defaults to "default".
    #[serde(default)]
    pub instance: Option<String>,
    /// Wait up to this long, 0 to 50 seconds, for a build or startup to finish. Defaults to 0.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

type LifecycleResult = Result<Json<Status>, ErrorData>;

#[tool_router(router = lifecycle_router, vis = "pub(super)")]
impl GameServer {
    #[tool(
        description = "Build and start the game editor, or report the one already running, and \
                       wait until it answers. Before you finish, stop the games you started \
                       (game_status lists them under `yours`) unless you leave one up for the \
                       user, and then say so.",
        meta = super::always_load()
    )]
    async fn game_start(&self, Parameters(params): Parameters<StartParams>) -> LifecycleResult {
        let lifecycle = self.lifecycle()?;
        self.check_pin(&params)?;
        let request = launch_request(params, false);
        let status = lifecycle
            .launcher
            .start(request, &lifecycle.caller)
            .await
            .map_err(fail)?;
        self.after_launch(status).await
    }

    #[tool(
        description = "Rebuild and restart a game you started, keeping the old one running until \
                       the new build succeeds (on Windows it is stopped before the build); launch \
                       params you leave out are reused from the last launch."
    )]
    async fn game_restart(&self, Parameters(params): Parameters<RestartParams>) -> LifecycleResult {
        let lifecycle = self.lifecycle()?;
        self.check_pin(&params.start)?;
        let request = launch_request(params.start, params.take_over);
        let status = lifecycle
            .launcher
            .restart(request, &lifecycle.caller)
            .await
            .map_err(fail)?;
        self.after_launch(status).await
    }

    #[tool(
        description = "Stop a game you started: it exits by itself, or is killed after \
                       grace_seconds. take_over stops a game someone else started, only when the \
                       user asked."
    )]
    async fn game_stop(&self, Parameters(params): Parameters<StopParams>) -> LifecycleResult {
        let lifecycle = self.lifecycle()?;
        let status = lifecycle
            .launcher
            .stop(
                params.instance.as_deref().unwrap_or(DEFAULT_INSTANCE),
                params.take_over,
                params.grace_seconds.unwrap_or(crate::launch::DEFAULT_GRACE),
                &lifecycle.caller,
            )
            .await
            .map_err(fail)?;
        // Only the stop itself failing counts: an earlier crash it reports is not this call's.
        if status.error.is_some() {
            return Err(failure(&status));
        }
        Ok(Json(with_warning(status)))
    }

    #[tool(
        description = "Where the game is: stopped, building, build_failed, starting, ready, \
                       exited (with crash details), external or port_in_use; `yours` lists the \
                       games you started.",
        meta = super::always_load()
    )]
    async fn game_status(&self, Parameters(params): Parameters<StatusParams>) -> LifecycleResult {
        let lifecycle = self.lifecycle()?;
        let wait = wait_for(params.wait_seconds, Duration::ZERO);
        let status = lifecycle
            .launcher
            .status(
                params.instance.as_deref().unwrap_or(DEFAULT_INSTANCE),
                &lifecycle.caller,
                wait,
            )
            .await
            .map_err(fail)?;
        Ok(Json(with_warning(status)))
    }
}

impl GameServer {
    fn lifecycle(&self) -> Result<&Lifecycle, ErrorData> {
        self.lifecycle.as_deref().ok_or_else(|| {
            fail(anyhow::anyhow!(
                "this session has no game launcher: `cargo metadata` failed in the sidecar's \
                 working directory, or its Cargo.toml has no `[workspace.metadata.ename.launch]`. \
                 The sidecar's log says which."
            ))
        })
    }

    /// `ENAME_BRP_URL` pins the session to one game; a launch on another port would never be
    /// reached.
    fn check_pin(&self, params: &StartParams) -> Result<(), ErrorData> {
        let Some(pinned) = self.endpoint.pinned() else {
            return Ok(());
        };
        let instance = params.instance.as_deref().unwrap_or(DEFAULT_INSTANCE);
        let wanted = params
            .port
            .or((instance == DEFAULT_INSTANCE).then_some(crate::launch::DEFAULT_PORT));
        let pinned_port = pinned
            .trim_end_matches('/')
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<u16>().ok());
        if wanted.is_some() && wanted == pinned_port {
            return Ok(());
        }
        Err(fail(anyhow::anyhow!(
            "ENAME_BRP_URL pins this session to {pinned}, so a game on another port would not be \
             reachable from it. Start the instance that uses that port, or unset ENAME_BRP_URL."
        )))
    }

    /// Ready: refresh the command catalogue at once so the game's tools appear. A failure is a
    /// tool error with the summary in its text.
    async fn after_launch(&self, status: Status) -> LifecycleResult {
        if status.state == GameState::Ready {
            self.catalog
                .refresh_and_notify(&self.endpoint.snapshot())
                .await;
        }
        if status.is_failure() {
            return Err(failure(&status));
        }
        Ok(Json(with_warning(status)))
    }
}

fn launch_request(params: StartParams, take_over: bool) -> LaunchRequest {
    LaunchRequest {
        instance: params
            .instance
            .unwrap_or_else(|| DEFAULT_INSTANCE.to_owned()),
        port: params.port,
        update: params.launch,
        take_over,
        wait: wait_for(params.wait_seconds, MAX_BLOCK),
    }
}

/// `wait_seconds`, clamped to 0 to 50.
fn wait_for(seconds: Option<f64>, default: Duration) -> Duration {
    match seconds {
        Some(seconds) if seconds.is_finite() => {
            Duration::from_secs_f64(seconds.clamp(0.0, MAX_BLOCK.as_secs_f64()))
        }
        _ => default,
    }
}

/// A successful result's staleness warning. A failure gets the full text from
/// `GameServer::call_tool`, after its summary.
fn with_warning(mut status: Status) -> Status {
    status.warning = staleness::warning();
    status
}

/// A failed launch: the one-line summary, then the whole result.
fn failure(status: &Status) -> ErrorData {
    let json = serde_json::to_string_pretty(status).unwrap_or_default();
    ErrorData::internal_error(format!("{}\n\n{json}", status.summary()), None)
}
