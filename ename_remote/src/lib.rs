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
//! | `game.pid.get` | Nothing in BRP says which run of the process answered, so a restart between two calls is invisible. Also answers the OS process id (`os_pid`), so a launcher can tell its game from another one on the port, and the run's log tally (`log`), so a client notices errors it did not look for. Every command and job answer carries the same `run` and `log`. |
//! | `game.exit` | A graceful stop with exit code 0 on every platform; Ctrl+C cannot reach a detached Windows process. |
//! | `game.command.list` / `.call` | The commands any crate registered in `ename_engine::command`, listed with their schemas and called by name. This crate never learns what they are. |
//! | `game.job.status` / `.list` / `.cancel` | The jobs those commands start. |
//! | `game.registry.describe` | `registry.schema` describes reflection structure, not the wire form, and is the whole registry at once. This answers a one-line shape and an example the game reads back. |
//! | `game.components.*`, `game.resources.*`, `game.entities.spawn` | BRP takes ids and full paths and fails on the first wrong value with serde's message. These take `name` or `entity`, short type names, coerce mistakes with one fix, and name the failing field with an example. Removing needs no reflection. |
//!
//! It also replaces `world.mutate_components` with a guarded version. See
//! [`components::guarded_mutate_components`].
//!
//! # Ports
//!
//! The server listens on `ENAME_BRP_PORT` (default 15702) and the render world's on the next
//! port. A taken port exits the game with code 3 instead of leaving it silently unreachable. See
//! [`port`].
//!
//! # Window size
//!
//! `ENAME_WINDOW_SIZE` (`WIDTHxHEIGHT`, physical pixels) sizes the primary window, so two launches
//! give the same view sizes. See [`window_size`].
//!
//! # Run record
//!
//! A small file per port says which run holds it, its latest panic and how it exited, so a client
//! that can no longer reach the game can say how it died. See [`run_record`].
//!
//! Screenshots, video and the editor's views are not here. Their crates register them as commands,
//! and they arrive through `game.command.*` like any other.

mod coerce;
mod command;
mod components;
mod entities;
mod exit;
mod logs;
mod overview;
mod port;
mod position;
mod registry;
mod run_id;
mod run_record;
mod time;
mod value_shape;
mod window_size;

use bevy::{
    app::PluginGroupBuilder,
    prelude::*,
    remote::{RemotePlugin, http::RemoteHttpPlugin},
};

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
            .with_method_main(registry::DESCRIBE_METHOD, registry::describe)
            .with_method_main(components::LIST_METHOD, components::list)
            .with_method_main(components::GET_METHOD, components::get)
            .with_method_main(components::INSERT_METHOD, components::insert)
            .with_method_main(components::MUTATE_METHOD, components::mutate)
            .with_method_main(components::REMOVE_METHOD, components::remove)
            .with_method_main(components::RESOURCES_GET_METHOD, components::resources_get)
            .with_method_main(
                components::RESOURCES_MUTATE_METHOD,
                components::resources_mutate,
            )
            .with_method_main(components::SPAWN_METHOD, components::spawn)
            .with_method_main(overview::METHOD, overview::overview)
            .with_method_main(position::GET_METHOD, position::get)
            .with_method_main(position::SET_METHOD, position::set)
            .with_method_main(time::GET_METHOD, time::get)
            .with_method_main(time::SET_METHOD, time::set)
            .with_method_main(logs::GET_METHOD, logs::get)
            .with_method_main(run_id::GET_METHOD, run_id::get)
            .with_method_main(exit::METHOD, exit::exit)
            .with_method_main(command::LIST_METHOD, command::list)
            .with_method_main(command::CALL_METHOD, command::call)
            .with_method_main(command::JOB_STATUS_METHOD, command::job_status)
            .with_method_main(command::JOB_LIST_METHOD, command::job_list)
            .with_method_main(command::JOB_CANCEL_METHOD, command::job_cancel)
            // Last, so it replaces the built-in of the same name.
            .with_method_main(
                components::BUILTIN_MUTATE_METHOD,
                components::guarded_mutate_components,
            );

        let port = port::brp_port();
        PluginGroupBuilder::start::<Self>()
            .add(remote)
            .add(RemoteHttpPlugin::default().with_port(port))
            // After the HTTP plugin, so its render-world port replaces the default one.
            .add(port::BrpPortPlugin { port })
            // After the port check, which exits first when another game holds the port, so the
            // record of that game is left alone.
            .add(run_record::RunRecordPlugin {
                path: run_record::record_path(port),
            })
            .add(exit::ExitPlugin)
            .add(window_size::WindowSizePlugin)
    }
}
