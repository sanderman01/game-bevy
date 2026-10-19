//! Physics transform compatibility for big_space grids.
//!
//! Replaces avian3d's built-in `position_to_transform` functionality to allow
//! Rigidbody entities as children inside a big_space grid, and gracefully
//! dealing with grid cell transitions and shifts in global transform.
//!
//! Same class of problem as the editor's gizmo grid anchor, and the same shape of fix: read
//! the offset big_space applied and put it through to the value that is out of date.
//!
//! For a body directly under a grid, the offset is against the `GlobalTransform`
//! [`super::grid_propagate`] set before the step, so it is exactly this tick's motion.

use avian3d::prelude::{Position, RigidBody, Rotation};
use bevy::prelude::*;

/// avian's own `position_to_transform` filter.
type StepMoved = (With<RigidBody>, Or<(Changed<Position>, Changed<Rotation>)>);

pub(crate) fn physics_position_to_transform(
    mut query: Query<(&mut Transform, &Position, &Rotation, &GlobalTransform), StepMoved>,
) {
    for (mut transform, position, rotation, global_transform) in &mut query {
        let offset = position.0 - global_transform.translation();
        // Compared first: a needless write would make `grid_propagate` recompute a static body
        // every tick.
        transform.set_if_neq(Transform {
            translation: transform.translation + offset,
            rotation: rotation.0,
            ..*transform
        });
    }
}
