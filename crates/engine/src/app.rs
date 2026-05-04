use bevy::{prelude::*, transform::TransformPlugin, window::WindowResolution};
use big_space::{camera::camera_controller, plugin::BigSpaceDefaultPlugins};
use modloader::LoaderState;

use crate::{
    camera_controller::custom_big_space_camera_inputs,
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
    .add_systems(
        PostUpdate,
        custom_big_space_camera_inputs.before(camera_controller),
    )
    .add_plugins(editor::editor::EditorPluginGroup)
    .add_plugins(modloader::ModLoaderPlugin::default())
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
    mut loader_state: ResMut<State<LoaderState>>,
    mut game_state: ResMut<NextState<GameState>>,
) {
    info!("check_loading");
    if matches!(loader_state.get(), LoaderState::AssetsRegistered) {
        game_state.set(GameState::Scene);
    }
}
