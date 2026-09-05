use bevy::prelude::*;
use big_space::camera::{BigSpaceCameraInput, camera_controller};

/// Ordering points for the fly camera.
///
/// This is the only place in the workspace that names `big_space::camera::camera_controller`.
/// Everything else orders against this set, so a big_space upgrade that renames or splits that
/// system is a one-line fix here.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlyCameraSystems {
    /// Writes `BigSpaceCameraInput` for this frame, before big_space consumes it.
    Apply,
}

/// Drives big_space's fly camera from keyboard and mouse.
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            PostUpdate,
            FlyCameraSystems::Apply.before(camera_controller),
        )
        .add_systems(
            PostUpdate,
            write_big_space_camera_input.in_set(FlyCameraSystems::Apply),
        );
    }
}

/// Keyboard and mouse bindings for the fly camera. Based on
/// `big_space::camera::default_camera_inputs`; the difference is that movement only happens
/// while the right mouse button is held.
fn write_big_space_camera_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse_button: Res<ButtonInput<MouseButton>>,
    mut mouse_move: MessageReader<bevy::input::mouse::MouseMotion>,
    mut cam: ResMut<BigSpaceCameraInput>,
) {
    cam.defaults_disabled = true;

    cam.reset();

    if !mouse_button.pressed(MouseButton::Right) {
        return;
    }

    keyboard.pressed(KeyCode::KeyW).then(|| cam.forward -= 1.0);
    keyboard.pressed(KeyCode::KeyS).then(|| cam.forward += 1.0);
    keyboard.pressed(KeyCode::KeyA).then(|| cam.right -= 1.0);
    keyboard.pressed(KeyCode::KeyD).then(|| cam.right += 1.0);
    keyboard.pressed(KeyCode::Space).then(|| cam.up += 1.0);
    keyboard
        .pressed(KeyCode::ControlLeft)
        .then(|| cam.up -= 1.0);
    keyboard.pressed(KeyCode::KeyQ).then(|| cam.roll += 2.0);
    keyboard.pressed(KeyCode::KeyE).then(|| cam.roll -= 2.0);
    keyboard
        .pressed(KeyCode::ShiftLeft)
        .then(|| cam.boost = true);
    if let Some(total_mouse_motion) = mouse_move.read().map(|e| e.delta).reduce(|sum, i| sum + i) {
        cam.pitch += total_mouse_motion.y as f64 * -0.1;
        cam.yaw += total_mouse_motion.x as f64 * -0.1;
    }
}
