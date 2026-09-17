//! Collider transform compatibility for big_space grids.
//!
//! avian seeds a collider's [`ColliderTransform`] by multiplying local [`Transform`]s up the
//! `ChildOf` chain. big_space keeps part of an entity's position in [`CellCoord`], outside
//! `Transform`, so that walk comes up short by one cell origin for every collider under a body
//! that spawned outside cell zero. The collider then sits a whole cell away from its body, and
//! the center of mass and angular inertia derived from it are wrong for the life of the entity.
//!
//! Differences between two `GlobalTransform`s in one grid are cell-correct, so derive the
//! collider's body-relative transform from those instead.
//!
//! Only newly attached colliders need this. avian's own propagation resets the accumulator at
//! the rigid body, so every value it writes is already cell-correct; the seeded one is wrong
//! and sticks around because a collider parked under its body never changes `Transform` or
//! `ChildOf` to trigger a rewrite.

use avian3d::prelude::{ColliderOf, ColliderTransform, Rotation};
use bevy::prelude::*;

/// Matches a collider on the tick it gains its transform or its body, whichever lands last.
type NewlyAttached = Or<(Added<ColliderTransform>, Added<ColliderOf>)>;

pub(crate) fn collider_transform_from_global(
    mut colliders: Query<(&mut ColliderTransform, &GlobalTransform, &ColliderOf), NewlyAttached>,
    bodies: Query<&GlobalTransform>,
) {
    for (mut collider_transform, collider_global, collider_of) in &mut colliders {
        let Ok(body_global) = bodies.get(collider_of.body) else {
            continue;
        };

        let relative = body_global.affine().inverse() * collider_global.affine();
        let (scale, rotation, translation) = relative.to_scale_rotation_translation();

        collider_transform.set_if_neq(ColliderTransform {
            translation,
            rotation: Rotation::from(rotation),
            scale,
        });
    }
}
