//! `ename_game` -- gameplay: game states and the starting scene.
//!
//! Sits above `ename_engine` and the asset crates and below the binary. It never reaches back
//! down into the editor. See `docs/design/crate-layout.md`.

pub mod scene;
mod state;

pub use state::{GameState, GameStatePlugin};

use bevy::{app::PluginGroupBuilder, prelude::*};

/// Everything the game contributes to an `App`.
///
/// Requires the `alias://` asset source the scene loads through. `EnginePlugins` registers it, so
/// any target that adds `EnginePlugins` has it; `GamePlugins` does not add it itself.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(state::GameStatePlugin)
            .add(scene::ScenePlugin)
    }
}
