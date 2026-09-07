//! `ename_remote` -- the agent-facing side channel: a BRP server plus the custom methods that
//! BRP's built-ins do not cover.
//!
//! Sits above `ename_engine` and `ename_game`, below the binary, and links into a target only
//! when that target asks for it. See `docs/design.md`.
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
//! | `game.run_state.get` / `.set` | Nothing in BRP can freeze the world, and reading a world that is still advancing answers a different question. |
//! | `game.logs.get` | Bevy's logs go to stderr, which a JSON-RPC client cannot see. |
//! | `game.pid.get` | Nothing in BRP says which run of the process answered, so a restart between two calls is invisible. |
//!
//! It also replaces `world.mutate_components` with a guarded version. See [`mutate`].

mod entities;
mod logs;
mod mutate;
mod position;
mod run;
mod run_id;

use bevy::{
    app::PluginGroupBuilder,
    prelude::*,
    remote::{RemotePlugin, http::RemoteHttpPlugin},
};

pub use logs::capture_layer;

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
/// Pair it with [`capture_layer`] on the target's `LogPlugin`, or `game.logs.get` returns
/// nothing: the layer has to be installed before the `App` exists.
pub struct RemotePlugins;

impl PluginGroup for RemotePlugins {
    fn build(self) -> PluginGroupBuilder {
        let remote = RemotePlugin::default()
            .with_method_main(entities::LIST_METHOD, entities::list)
            .with_method_main(position::GET_METHOD, position::get)
            .with_method_main(position::SET_METHOD, position::set)
            .with_method_main(run::GET_METHOD, run::get)
            .with_method_main(run::SET_METHOD, run::set)
            .with_method_main(logs::GET_METHOD, logs::get)
            .with_method_main(run_id::GET_METHOD, run_id::get)
            // Last, so it replaces the built-in of the same name.
            .with_method_main(mutate::METHOD, mutate::mutate_components);

        PluginGroupBuilder::start::<Self>()
            .add(remote)
            .add(RemoteHttpPlugin::default())
            .add(run::RunControlPlugin)
            .add(run_id::RunIdPlugin)
    }
}
