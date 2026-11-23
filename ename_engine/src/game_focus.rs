//! Whether the pointer and the keyboard are the game's, for a host that shows the game in part of
//! its window beside its own controls.

use bevy::prelude::*;

/// Registers [`PointerOverGame`], [`KeyboardOnGame`] and [`KeyboardOnSceneView`].
pub struct GameFocusPlugin;

impl Plugin for GameFocusPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<PointerOverGame>()
            .register_type::<KeyboardOnGame>()
            .init_resource::<PointerOverGame>()
            .register_type::<KeyboardOnSceneView>()
            .init_resource::<KeyboardOnGame>()
            .init_resource::<KeyboardOnSceneView>();
    }
}

/// Whether the pointer is over the game's view, so a click or scroll there is meant for the game.
///
/// True by default: a game alone in its window owns every pointer. A host that draws the game into
/// part of the window, such as the editor's Game View, keeps it current; a game reads it before it
/// grabs the cursor or acts on a mouse button.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource)]
pub struct PointerOverGame(pub bool);

impl Default for PointerOverGame {
    fn default() -> Self {
        Self(true)
    }
}

/// Whether key presses are meant for the game, rather than for one of the host's own controls.
///
/// True by default: a game alone in its window owns the keyboard. A host with controls of its own,
/// such as the editor, keeps it current; a game reads it before it acts on a key.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource)]
pub struct KeyboardOnGame(pub bool);

impl Default for KeyboardOnGame {
    fn default() -> Self {
        Self(true)
    }
}

/// Whether key presses are meant for the editor's Scene View: its tab has focus and no control
/// takes text. A game may read it for editor-only keys it handles while the Scene View is shown.
///
/// False by default: without an editor there is no Scene View. The editor keeps it current.
#[derive(Resource, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource)]
pub struct KeyboardOnSceneView(pub bool);
