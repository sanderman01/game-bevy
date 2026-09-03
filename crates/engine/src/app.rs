use avian3d::{
    PhysicsPlugins, debug_render::PhysicsDebugPlugin, physics_transform::PhysicsTransformConfig,
    prelude::PhysicsSystems,
};
use bevy::{
    prelude::*,
    transform::{TransformPlugin, TransformSystems},
    window::WindowResolution,
};
use big_space::{
    camera::camera_controller,
    plugin::{BigSpaceDebugPlugins, BigSpaceDefaultPlugins, BigSpaceSystems},
};
use editor::editor::EditorPluginGroup;
use modloader::{LoaderState, ModLoaderPlugin};

use crate::{
    camera::VirtualCameraPlugin,
    camera_controller::custom_big_space_camera_inputs,
    gizmo_compat::reanchor_gizmo_drag_across_cells,
    physics_compat::physics_position_to_transform,
    scene::{load_model, new_simple_scene},
};

pub fn create_app(app: &mut bevy::app::App) {
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "My Bevy Game".into(),
                    resolution: WindowResolution::new(1280, 720),
                    ..default()
                }),
                ..default()
            })
            .disable::<TransformPlugin>(),
    )
    .add_plugins(BigSpaceDefaultPlugins)
    // No longer bundled with BigSpaceDefaultPlugins as of big_space 0.13.
    .add_plugins(BigSpaceDebugPlugins::default())
    .add_systems(
        PostUpdate,
        custom_big_space_camera_inputs.before(camera_controller),
    )
    .add_systems(
        PostUpdate,
        // Runs once big_space has moved the entity into its new cell, and so before the
        // gizmo reads its drag snapshot again next frame.
        reanchor_gizmo_drag_across_cells
            .after(BigSpaceSystems::RecenterLargeTransforms)
            .after(TransformSystems::Propagate),
    )
    .add_plugins(VirtualCameraPlugin)
    .add_plugins(EditorPluginGroup)
    .add_plugins(ModLoaderPlugin::default())
    .add_plugins(PhysicsPlugins::default())
    .add_plugins(PhysicsDebugPlugin)
    .insert_resource(PhysicsTransformConfig {
        propagate_before_physics: false,
        transform_to_position: true,
        position_to_transform: false, // disabled -- we use physics_compat::physics_position_to_transform instead
        ..default()
    })
    .add_systems(
        FixedPostUpdate,
        physics_position_to_transform.after(PhysicsSystems::Writeback),
    )
    .insert_state(GameState::Loading)
    .add_systems(Update, check_loading.run_if(in_state(GameState::Loading)))
    .add_systems(OnEnter(GameState::Scene), new_simple_scene)
    .add_systems(
        OnEnter(GameState::Scene),
        load_model
            .after(new_simple_scene)
            .run_if(in_state(GameState::Scene)),
    );
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, States)]
enum GameState {
    #[default]
    Loading,
    Scene,
    Play,
}

fn check_loading(
    loader_state: ResMut<State<LoaderState>>,
    mut game_state: ResMut<NextState<GameState>>,
) {
    info!("check_loading");
    if matches!(loader_state.get(), LoaderState::AssetsRegistered) {
        game_state.set(GameState::Scene);
    }
}
