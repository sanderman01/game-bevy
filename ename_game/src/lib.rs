//! `ename_game` -- gameplay lib.
//!
//! This is where game-specific functionality goes.
//! Sits above `ename_engine` and the asset crates and below the binary.
//! It never reaches into the editor.
//! See `docs/design/crate-layout.md`.

use bevy::{app::PluginGroupBuilder, prelude::*};
use ename_engine::stage::open_stage;

/// A group of plugins comprising your game functionality.
/// Add this plugin group to the app in your game binary crate.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>().add(StartPlugin)
    }
}

/// Opens the starting stage on startup.
/// (a stage is a scene or world, much like a Unity scene or Unreal map file)
pub struct StartPlugin;

impl Plugin for StartPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, open_start_stage);
    }
}

fn open_start_stage(world: &mut World) {
    open_stage(world, "alias://examples::stage");
}
