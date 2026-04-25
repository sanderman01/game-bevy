use bevy::prelude::*;

/// Provides sensible keyboard and mouse input.
/// Based on big_space::camera::default_camera_inputs.
/// Main difference is disabling camera movement unless holding the right mouse button.
pub fn custom_big_space_camera_inputs(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse_button: Res<ButtonInput<MouseButton>>,
    mut mouse_move: MessageReader<bevy::input::mouse::MouseMotion>,
    mut cam: ResMut<big_space::camera::BigSpaceCameraInput>,
) {
    cam.defaults_disabled = true;

    cam.reset();

    //let mouse_right = mouse_button.read().any(|e| e.state.is_held() && e.button == bevy_input::mouse::MouseButton::Right);
    let mouse_right = mouse_button.pressed(MouseButton::Right);
    if !mouse_right {
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
