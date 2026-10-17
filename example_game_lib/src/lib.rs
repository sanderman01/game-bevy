//! `example_game_lib` -- gameplay lib.
//!
//! This is where game-specific functionality goes.
//! Sits above `ename_engine` and the asset crates and below the binary.
//! It never reaches into the editor.
//! See `docs/design/crate-layout.md`.

pub mod airship;
pub mod sky;

use bevy::{app::PluginGroupBuilder, prelude::*};
use ename_engine::{camera::ground::GroundSurface, stage::open_stage};

/// A group of plugins comprising your game functionality.
/// Add this plugin group to the app in your game binary crate.
pub struct GamePlugins;

impl PluginGroup for GamePlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(StartPlugin)
            .add(airship::AirshipMovementPlugin)
            .add(sky::SkyPlugin)
    }
}

/// Opens the starting stage on startup.
/// (a stage is a scene or world, much like a Unity scene or Unreal map file)
pub struct StartPlugin;

impl Plugin for StartPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, open_start_stage)
            .add_systems(Update, mark_terrain);
    }
}

/// The start stage's terrain: a static body whose mesh colliders are the ground.
const TERRAIN: &str = "examples::map";

fn open_start_stage(world: &mut World) {
    open_stage(world, "alias://examples::stage");
}

/// Marks the stage's terrain as [`GroundSurface`]: its collider for the physics probe, and the
/// render meshes under it for the mesh probe. A camera placed under it is lifted onto it.
fn mark_terrain(mut commands: Commands, named: Query<(Entity, &Name), Added<Name>>) {
    for (entity, name) in &named {
        if name.as_str() == TERRAIN {
            commands.entity(entity).insert(GroundSurface);
        }
    }
}
