//! Whether the pointer and the keyboard are the game's, for a host that shows the game in part of
//! its window beside its own controls.

use bevy::{
    camera::RenderTarget,
    input::InputSystems,
    prelude::*,
    window::{PrimaryWindow, WindowRef},
};

use crate::camera::MainCameraFilter;

/// Registers [`PointerOverGame`], [`PointerInGameView`], [`KeyboardOnGame`] and
/// [`KeyboardOnSceneView`], and keeps [`PointerInGameView`] current for a Main Camera that renders
/// to a window.
pub struct GameFocusPlugin;

impl Plugin for GameFocusPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<PointerOverGame>()
            .register_type::<PointerInGameView>()
            .register_type::<KeyboardOnGame>()
            .init_resource::<PointerOverGame>()
            .init_resource::<PointerInGameView>()
            .register_type::<KeyboardOnSceneView>()
            .init_resource::<KeyboardOnGame>()
            .init_resource::<KeyboardOnSceneView>()
            // After this frame's cursor moves are known.
            .add_systems(PreUpdate, track_window_pointer.after(InputSystems));
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

/// Where the pointer is in the game's view, as a fraction of its size from the top left: (0, 0)
/// top left, (1, 1) bottom right. `None` while the pointer is not over the view.
///
/// For a Main Camera that renders to a window, [`GameFocusPlugin`] keeps it current from the
/// window's cursor. A host that paints the camera's image elsewhere, such as the editor's Game
/// View, keeps it current itself.
#[derive(Resource, Reflect, Debug, Default, Clone, Copy, PartialEq)]
#[reflect(Resource)]
pub struct PointerInGameView(pub Option<Vec2>);

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

/// Follows the window cursor over an active Main Camera that renders to a window, as a fraction of
/// its viewport. A camera rendering elsewhere is left to its host.
fn track_window_pointer(
    cameras: Query<(&Camera, &RenderTarget), MainCameraFilter>,
    primary: Query<&Window, With<PrimaryWindow>>,
    windows: Query<&Window>,
    mut pointer: ResMut<PointerInGameView>,
) {
    let Some((camera, window)) = cameras.iter().find_map(|(camera, target)| {
        let window = match target {
            RenderTarget::Window(WindowRef::Primary) => primary.single().ok(),
            RenderTarget::Window(WindowRef::Entity(entity)) => windows.get(*entity).ok(),
            _ => None,
        };
        camera.is_active.then_some((camera, window?))
    }) else {
        return;
    };
    // From the window rather than the camera's computed target, which lags a resize by a frame.
    let rect = match &camera.viewport {
        Some(viewport) => {
            let min = viewport.physical_position.as_vec2() / window.scale_factor();
            let size = viewport.physical_size.as_vec2() / window.scale_factor();
            Rect::from_corners(min, min + size)
        }
        None => Rect::from_corners(Vec2::ZERO, window.size()),
    };
    let fraction = window
        .cursor_position()
        .map(|cursor| (cursor - rect.min) / rect.size())
        .filter(|fraction| fraction.cmpge(Vec2::ZERO).all() && fraction.cmple(Vec2::ONE).all());
    pointer.set_if_neq(PointerInGameView(fraction));
}

#[cfg(test)]
mod tests {
    use bevy::window::WindowResolution;

    use super::*;
    use crate::camera::MainCamera;

    fn app(viewport: Option<bevy::camera::Viewport>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(bevy::input::InputPlugin)
            .add_plugins(GameFocusPlugin);
        let mut window = Window {
            resolution: WindowResolution::new(200, 100).with_scale_factor_override(1.0),
            ..default()
        };
        window.set_cursor_position(Some(Vec2::new(150.0, 25.0)));
        app.world_mut().spawn((window, PrimaryWindow));
        app.world_mut().spawn((
            MainCamera,
            Camera {
                viewport,
                ..default()
            },
            RenderTarget::Window(WindowRef::Primary),
        ));
        app
    }

    #[test]
    fn window_cursor_is_a_fraction_of_the_view() {
        let mut app = app(None);
        app.update();
        assert_eq!(
            app.world().resource::<PointerInGameView>().0,
            Some(Vec2::new(0.75, 0.25))
        );
    }

    #[test]
    fn cursor_outside_the_viewport_is_none() {
        let mut app = app(Some(bevy::camera::Viewport {
            physical_position: UVec2::ZERO,
            physical_size: UVec2::new(100, 100),
            ..default()
        }));
        app.update();
        assert_eq!(app.world().resource::<PointerInGameView>().0, None);
    }
}
