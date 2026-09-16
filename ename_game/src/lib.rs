//! `ename_game` -- gameplay: the starting stage.
//!
//! Sits above `ename_engine` and the asset crates and below the binary. It never reaches back
//! down into the editor. See `docs/design/crate-layout.md`.

pub mod stage;

use bevy::{app::PluginGroupBuilder, prelude::*};

/// Everything the game contributes to an `App`.
///
/// Requires the `alias://` asset source the stage loads through. `EnginePlugins` registers it, so
/// any target that adds `EnginePlugins` has it; `GamePlugins` does not add it itself.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>().add(stage::StagePlugin)
    }
}
