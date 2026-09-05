//! [`GameState`] and the plugin that advances it out of `Loading` once content has loaded.

use bevy::prelude::*;
use ename_content::LoaderState;

/// Where the game is in its own lifecycle. The engine has no opinion on whether a game has a
/// `Play` state, so this lives here.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, States)]
pub enum GameState {
    #[default]
    Loading,
    Scene,
    Play,
}

/// Owns [`GameState`] and the transition out of `Loading`.
pub struct GameStatePlugin;

impl Plugin for GameStatePlugin {
    fn build(&self, app: &mut App) {
        app.insert_state(GameState::Loading).add_systems(
            Update,
            enter_scene_when_assets_registered.run_if(in_state(GameState::Loading)),
        );
    }
}

/// The scene references assets by package alias, so it cannot spawn until the content layer
/// has finished registering them.
fn enter_scene_when_assets_registered(
    loader_state: Res<State<LoaderState>>,
    mut game_state: ResMut<NextState<GameState>>,
) {
    if matches!(loader_state.get(), LoaderState::AssetsRegistered) {
        game_state.set(GameState::Scene);
    }
}
