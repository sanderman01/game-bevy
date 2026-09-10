//! Physics transform compatibility for big_space grids.
//!
//! Replaces avian3d's built-in `position_to_transform` functionality to allow
//! Rigidbody entities as children inside a big_space grid, and gracefully
//! dealing with grid cell transitions and shifts in global transform.
//!
//! Same class of problem as the editor's gizmo grid anchor, and the same shape of fix: read
//! the offset big_space applied and put it through to the value that is out of date.

use avian3d::prelude::{Position, RigidBody, Rotation};
use bevy::prelude::*;

pub(crate) fn physics_position_to_transform(
    mut query: Query<(&mut Transform, &Position, &Rotation, &GlobalTransform), With<RigidBody>>,
) {
    for (mut transform, position, rotation, global_transform) in &mut query {
        let offset = position.0 - global_transform.translation();
        transform.translation += offset;
        transform.rotation = rotation.0;
    }
}
