//! The starting scene, loaded from `assets/basegame/core/start_scene.scn.ron` via the
//! alias `core::start_scene`. See `scratch/scenes-spec.md`.

use bevy::prelude::*;
use ename_engine::scene::{DynamicWorldFormat, SceneAppExt, open_scene_by_alias};

use crate::GameState;

/// Opens the starting scene on entry to [`GameState::Scene`].
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.register_scene_format(DynamicWorldFormat)
            .add_systems(OnEnter(GameState::Scene), open_start_scene);
    }
}

fn open_start_scene(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    formats: Res<ename_engine::scene::SceneFormats>,
) {
    open_scene_by_alias(&mut commands, &asset_server, &formats, "core::start_scene");
}
