//! [`GameState`] and the plugin that owns it.

use bevy::prelude::*;

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
        app.insert_state(GameState::Loading)
            .add_systems(Startup, enter_scene);
    }
}

/// The scene addresses assets through `alias://`, which resolves inside `bevy_asset`, so there is
/// nothing to wait for. A loading screen later gates on `AssetServer::load_state` for the handles
/// it cares about rather than on another crate's bookkeeping.
fn enter_scene(mut game_state: ResMut<NextState<GameState>>) {
    game_state.set(GameState::Scene);
}
