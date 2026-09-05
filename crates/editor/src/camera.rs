//! Viewport camera bindings.
//!
//! The editor owns which keys fly the camera; the engine owns what flying means. See
//! `engine::input`.

use bevy::{input::mouse::MouseMotion, prelude::*};
use engine::input::{FlyCameraIntent, FlyCameraSystems};

/// True while the fly camera is claiming the keyboard.
///
/// The camera and the gizmo shortcuts both want W and E. The camera wins while the right mouse
/// button is held, and this is the single place that rule is written down.
pub(crate) fn fly_camera_active(mouse: Res<ButtonInput<MouseButton>>) -> bool {
    mouse.pressed(MouseButton::Right)
}

/// Maps keyboard and mouse to [`FlyCameraIntent`].
pub struct EditorCameraPlugin;

impl Plugin for EditorCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            write_fly_camera_intent.in_set(FlyCameraSystems::Intent),
        );
    }
}

/// Same bindings as before: WASD plus space and left control, Q and E to roll, left shift to
/// boost, mouse to look. Based on `big_space::camera::default_camera_inputs`.
fn write_fly_camera_intent(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse_button: Res<ButtonInput<MouseButton>>,
    mut mouse_move: MessageReader<MouseMotion>,
    mut intent: ResMut<FlyCameraIntent>,
) {
    if !mouse_button.pressed(MouseButton::Right) {
        // Drop the motion accumulated while not flying, or the first frame of the next drag
        // gets all of it at once.
        mouse_move.clear();
        return;
    }

    keyboard
        .pressed(KeyCode::KeyW)
        .then(|| intent.forward -= 1.0);
    keyboard
        .pressed(KeyCode::KeyS)
        .then(|| intent.forward += 1.0);
    keyboard.pressed(KeyCode::KeyA).then(|| intent.right -= 1.0);
    keyboard.pressed(KeyCode::KeyD).then(|| intent.right += 1.0);
    keyboard.pressed(KeyCode::Space).then(|| intent.up += 1.0);
    keyboard
        .pressed(KeyCode::ControlLeft)
        .then(|| intent.up -= 1.0);
    keyboard.pressed(KeyCode::KeyQ).then(|| intent.roll += 2.0);
    keyboard.pressed(KeyCode::KeyE).then(|| intent.roll -= 2.0);
    keyboard
        .pressed(KeyCode::ShiftLeft)
        .then(|| intent.boost = true);

    if let Some(total_mouse_motion) = mouse_move.read().map(|e| e.delta).reduce(|sum, i| sum + i) {
        intent.pitch += total_mouse_motion.y as f64 * -0.1;
        intent.yaw += total_mouse_motion.x as f64 * -0.1;
    }
}
