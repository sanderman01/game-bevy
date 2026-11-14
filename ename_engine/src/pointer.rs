//! Whether the pointer is over the game, for a host that shows the game in part of its window.

use bevy::prelude::*;

/// Registers [`PointerOverGame`].
pub struct PointerPlugin;

impl Plugin for PointerPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<PointerOverGame>()
            .init_resource::<PointerOverGame>();
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
