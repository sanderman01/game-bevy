//! `ename_game` -- gameplay: game states and the starting scene.
//!
//! Sits above `ename_engine` and `ename_content` and below the binary. It never reaches back
//! down into the editor. See `docs/design.md`.

pub mod scene;
mod state;

pub use state::GameState;

use bevy::{app::PluginGroupBuilder, prelude::*};

/// Everything the game contributes to an `App`.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(state::GameStatePlugin)
            .add(scene::ScenePlugin)
    }
}
