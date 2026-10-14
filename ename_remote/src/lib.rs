//! `ename_remote` -- the agent-facing side channel: a BRP server plus the custom methods that
//! BRP's built-ins do not cover.
//!
//! Sits above `ename_engine`, below the binary, and links into a target only when that target
//! asks for it. See `docs/design/agent-tooling.md`.
//!
//! # This crate is not safe to ship
//!
//! BRP is arbitrary read and write access to the running world over a loopback socket, with no
//! authentication. Localhost is not a trust boundary: any process running as the same user can
//! connect. The `agent` feature on the binary must be off in a shipping build, and absence of
//! this crate from the dependency graph is what makes that checkable.
//!
//! # What the custom methods add
//!
//! | Method | Why BRP cannot do it |
//! | --- | --- |
//! | `game.entities.list` | `world.query` filters by exact type path and cannot report a position that survives the floating origin. |
//! | `game.position.get` / `.set` | Position is a `CellCoord` plus a `Transform`, relative to a moving origin. A bare `Transform` is wrong everywhere but one cell. |
//! | `game.time.get` / `.set` | Nothing in BRP can freeze the world, and reading a world that is still advancing answers a different question. |
//! | `game.logs.get` | Bevy's logs go to stderr, which a JSON-RPC client cannot see. |
//! | `game.pid.get` | Nothing in BRP says which run of the process answered, so a restart between two calls is invisible. |
//! | `game.command.list` / `.call` | The commands any crate registered in `ename_engine::command`, listed with their schemas and called by name. This crate never learns what they are. |
//! | `game.job.status` / `.list` / `.cancel` | The jobs those commands start. |
//! | `game.screenshot.capture` / `.result` | Capture is a GPU readback that completes frames later, so it is a request plus a poll. |
//! | `game.video.record` / `.status` / `.stop` | A recording runs for many frames, so it is a request, a poll and an optional stop. Only with the `video` feature. |
//!
//! It also replaces `world.mutate_components` with a guarded version. See [`mutate`].
//!
//! `editor.view.get` / `.set`, `editor.screenshot.capture` and `editor.video.record` are not here. They need the
//! editor's dock and camera, which are private to it, so `ename_editor` registers them itself.

mod command;
mod entities;
mod logs;
mod mutate;
mod position;
mod run_id;
mod screenshot;
mod time;
#[cfg(feature = "video")]
mod video;

use bevy::{
    app::PluginGroupBuilder,
    camera::RenderTarget,
    prelude::*,
    remote::{BrpError, RemotePlugin, error_codes, http::RemoteHttpPlugin},
};
use ename_engine::camera::MainCamera;

/// Serializes a handler's response, turning a serialization failure into a BRP error.
///
/// Every custom method builds its response from a struct, so this is the one place that
/// conversion happens.
pub(crate) fn to_value<T: serde::Serialize>(value: T) -> bevy::remote::BrpResult {
    serde_json::to_value(value).map_err(|err| bevy::remote::BrpError {
        code: bevy::remote::error_codes::INTERNAL_ERROR,
        message: err.to_string(),
        data: None,
    })
}

/// The one camera the Game View would show. Same rule as the editor's `camera_available`.
pub(crate) fn main_camera(world: &mut World) -> Result<Entity, BrpError> {
    let cameras: Vec<Entity> = world
        .query_filtered::<Entity, (With<MainCamera>, With<Camera>, With<RenderTarget>)>()
        .iter(world)
        .collect();
    match cameras[..] {
        [camera] => Ok(camera),
        [] => Err(internal("the loaded stage has no MainCamera".to_owned())),
        _ => Err(internal(format!(
            "there are {} MainCameras, so which one is the game camera is ambiguous",
            cameras.len()
        ))),
    }
}

fn internal(message: String) -> BrpError {
    BrpError {
        code: error_codes::INTERNAL_ERROR,
        message,
        data: None,
    }
}

/// The BRP server and its custom methods, as a target adds it.
///
/// `game.logs.get` reads `ename_engine::log::LogBuffer`, and the command methods
/// `ename_engine::command`'s registry, both of which `EnginePlugins` always installs, so this group
/// needs nothing from the target beyond being added.
pub struct RemotePlugins;

impl PluginGroup for RemotePlugins {
    fn build(self) -> PluginGroupBuilder {
        let remote = RemotePlugin::default()
            .with_method_main(entities::LIST_METHOD, entities::list)
            .with_method_main(position::GET_METHOD, position::get)
            .with_method_main(position::SET_METHOD, position::set)
            .with_method_main(time::GET_METHOD, time::get)
            .with_method_main(time::SET_METHOD, time::set)
            .with_method_main(logs::GET_METHOD, logs::get)
            .with_method_main(run_id::GET_METHOD, run_id::get)
            .with_method_main(command::LIST_METHOD, command::list)
            .with_method_main(command::CALL_METHOD, command::call)
            .with_method_main(command::JOB_STATUS_METHOD, command::job_status)
            .with_method_main(command::JOB_LIST_METHOD, command::job_list)
            .with_method_main(command::JOB_CANCEL_METHOD, command::job_cancel)
            .with_method_main(screenshot::CAPTURE_METHOD, screenshot::capture)
            .with_method_main(screenshot::RESULT_METHOD, screenshot::result);
        #[cfg(feature = "video")]
        let remote = remote
            .with_method_main(video::RECORD_METHOD, video::record)
            .with_method_main(video::STATUS_METHOD, video::status)
            .with_method_main(video::STOP_METHOD, video::stop);
        let remote = remote
            // Last, so it replaces the built-in of the same name.
            .with_method_main(mutate::METHOD, mutate::mutate_components);

        PluginGroupBuilder::start::<Self>()
            .add(remote)
            .add(RemoteHttpPlugin::default())
    }
}
