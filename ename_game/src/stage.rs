//! The starting stage, loaded from `assets/basegame/core/start_stage.scn.ron` via the
//! alias `core::start_stage`. See `scratch/scenes-spec.md`.

use bevy::prelude::*;
use ename_asset_alias::ALIAS_SOURCE;
use ename_engine::stage::{DynamicWorldFormat, StageAppExt, open_stage};

/// Opens the starting stage on startup.
pub struct StagePlugin;

impl Plugin for StagePlugin {
    fn build(&self, app: &mut App) {
        app.register_stage_format(DynamicWorldFormat)
            .add_systems(Startup, open_start_stage);
    }
}

fn open_start_stage(world: &mut World) {
    open_stage(world, &format!("{ALIAS_SOURCE}://examples::stage"));
}
