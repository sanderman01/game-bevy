//! Airship movement: turns [`PlayerInput`] into forces on a rigid body with an [`AirshipMovement`] component.

use avian3d::prelude::{Forces, WriteRigidBodyForces};
use bevy::prelude::*;

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
    pub x: f32,
    /// Throttle: positive is forward (W), negative is backward (S).
    pub y: f32,
}

fn read_player_input(keys: Res<ButtonInput<KeyCode>>, mut input: ResMut<PlayerInput>) {
    let axis = |positive: KeyCode, negative: KeyCode| {
        f32::from(keys.pressed(positive)) - f32::from(keys.pressed(negative))
    };
    input.set_if_neq(PlayerInput {
        x: axis(KeyCode::KeyD, KeyCode::KeyA),
        y: axis(KeyCode::KeyW, KeyCode::KeyS),
    });
}

/// Makes a rigid body steerable by the player.
#[derive(Component, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Component, Default, Debug, PartialEq)]
pub struct AirshipMovement {
    /// Thrust along the entity's forward axis at full throttle, in newtons.
    pub thrust: f32,
    /// Yaw torque at full steering, in newton-metres.
    pub turn_torque: f32,
}

impl Default for AirshipMovement {
    fn default() -> Self {
        Self {
            thrust: 20_000.0,
            turn_torque: 5_000.0,
        }
    }
}

fn drive_airship(input: Res<PlayerInput>, mut airships: Query<(&AirshipMovement, Forces)>) {
    for (movement, mut forces) in &mut airships {
        // Forward is -Z and steering right is a negative yaw about +Y.
        forces.apply_local_force(Vec3::NEG_Z * (input.y * movement.thrust));
        forces.apply_local_torque(Vec3::NEG_Y * (input.x * movement.turn_torque));
    }
}
