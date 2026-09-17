//! Airship movement example: turns [`PlayerInput`] into forces on a rigid body with an [`AirshipMovement`] component.

use avian3d::{
    dynamics::rigid_body::forces::ReadRigidBodyForces,
    prelude::{Forces, WriteRigidBodyForces},
};
use bevy::prelude::*;

const KEY_RIGHT: KeyCode = KeyCode::KeyD;
const KEY_LEFT: KeyCode = KeyCode::KeyA;
const KEY_FORWARD: KeyCode = KeyCode::KeyW;
const KEY_REARWARD: KeyCode = KeyCode::KeyS;
const KEY_UP: KeyCode = KeyCode::Space;
const KEY_DOWN: KeyCode = KeyCode::ControlLeft;

const UP_DIR: Vec3 = vec3(0.0, 1.0, 0.0);

/// Registers [`AirshipMovement`] and the system that drives it from [`PlayerInput`].
pub struct AirshipMovementPlugin;

impl Plugin for AirshipMovementPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<PlayerInput>()
            .register_type::<AirshipMovement>()
            .init_resource::<PlayerInput>()
            .add_systems(Update, read_player_input)
            .add_systems(FixedUpdate, drive_airship);
    }
}

/// Movement axes from the keyboard, each in `-1.0..1.0`.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Resource, Default, Debug, PartialEq)]
pub struct PlayerInput {
    /// Steering: positive is right (D), negative is left (A).
    pub steering_axis: f32,
    /// Throttle: positive is forward (W), negative is backward (S).
    pub move_forward_axis: f32,
    /// Altitude: positive is up (Shift), negative is down (Ctrl)
    pub move_vertical_axis: f32,
}

fn read_player_input(keys: Res<ButtonInput<KeyCode>>, mut input: ResMut<PlayerInput>) {
    let axis = |positive: KeyCode, negative: KeyCode| {
        f32::from(keys.pressed(positive)) - f32::from(keys.pressed(negative))
    };

    input.set_if_neq(PlayerInput {
        steering_axis: axis(KEY_RIGHT, KEY_LEFT),
        move_forward_axis: axis(KEY_FORWARD, KEY_REARWARD),
        move_vertical_axis: axis(KEY_UP, KEY_DOWN),
    });
}

/// Makes a rigid body steerable by the player.
#[derive(Component, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Component, Default, Debug, PartialEq)]
pub struct AirshipMovement {
    /// Thrust along the entity's forward axis at full throttle, in newtons.
    pub thrust_forward: f32,
    /// Thrust along the entity's vertical axis at full throttle, in newtons.
    pub thrust_vertical: f32,
    /// Yaw torque at full steering, in newton-metres.
    pub turn_torque: f32,
    // Proportional gain, in newton-metres.
    pub align_stability: f32,
    // Derivative gain to stop wobble, in newton-metres.
    pub align_damping: f32,
}

impl Default for AirshipMovement {
    fn default() -> Self {
        Self {
            thrust_forward: 20_000.0,
            thrust_vertical: 20_000.0,
            turn_torque: 5_000.0,
            align_stability: 50_000.0,
            align_damping: 2_000.0,
        }
    }
}

fn drive_airship(input: Res<PlayerInput>, mut airships: Query<(&AirshipMovement, Forces)>) {
    for (movement, mut forces) in &mut airships {
        // Apply movement input forces
        // Forward is -Z and steering right is a negative yaw about +Y.
        forces.apply_local_force(Vec3::NEG_Z * (input.move_forward_axis * movement.thrust_forward));
        forces.apply_force(Vec3::Y * (input.move_vertical_axis * movement.thrust_vertical));
        forces.apply_local_torque(Vec3::NEG_Y * (input.steering_axis * movement.turn_torque));

        // Align back to y = up over time.
        let body_up = forces.rotation() * UP_DIR;
        let direction_error = Vec3::cross(body_up, UP_DIR);
        let torque = (direction_error * movement.align_stability)
            - (forces.angular_velocity() * movement.align_damping);
        forces.apply_torque(torque);
    }
}
