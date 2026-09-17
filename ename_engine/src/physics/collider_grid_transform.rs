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

use avian3d::prelude::{ColliderOf, ColliderTransform, Rotation};
use bevy::prelude::*;

pub(crate) fn collider_transform_from_global(
    mut colliders: Query<(&mut ColliderTransform, &GlobalTransform, &ColliderOf)>,
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
