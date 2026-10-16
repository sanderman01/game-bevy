//! `game.pid.get`: which run of the game process answered.
//!
//! Entity ids do not survive a restart, and neither does anything else the agent holds between
//! tool calls. Without a way to notice that the process changed, a plan made against one run
//! keeps being applied to the next one and the failures look like unrelated bugs. The id itself is
//! `ename_engine::command::RunId`, the same one every command answer and job id carries.

use bevy::{
    prelude::*,
    remote::{BrpError, BrpResult},
};
use ename_engine::command::RunId;
use serde::Serialize;
use serde_json::Value;

pub const GET_METHOD: &str = "game.pid.get";

#[derive(Serialize)]
struct PidResponse {
    pid: String,
    /// The operating system's id for this process, so a launcher can tell its own game from
    /// another one holding the port.
    os_pid: u32,
}

pub(crate) fn get(In(_): In<Option<Value>>, world: &mut World) -> BrpResult {
    let Some(run) = world.get_resource::<RunId>() else {
        return Err(BrpError::internal(
            "this app has no run id: add ename_engine's CommandPlugin",
        ));
    };
    crate::to_value(PidResponse {
        pid: run.as_str().to_owned(),
        os_pid: std::process::id(),
    })
}
