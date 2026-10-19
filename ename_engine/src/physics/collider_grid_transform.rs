//! Collider transform compatibility for big_space grids.
//!
//! avian seeds a collider's [`ColliderTransform`] by multiplying local [`Transform`]s up the
//! `ChildOf` chain. big_space keeps part of an entity's position in [`CellCoord`], outside
//! `Transform`, so that walk comes up short by one cell origin for every collider under a body
//! that spawned outside cell zero. The collider then sits a whole cell away from its body, and
//! the center of mass and angular inertia derived from it are wrong for the life of the entity.
//!
//! So recompute it the way avian's own propagation does, which resets the accumulator at the
//! rigid body and so never involves the cell: the body contributes only its scale accumulated
//! from the root, and each entity below it composes translation, rotation and scale separately
//! (componentwise scale, not a decomposed affine, which would shear under non-uniform scale).
//!
//! Only newly attached colliders need this. Every value avian's propagation writes is already
//! cell-correct; the seeded one is wrong and sticks around because a collider parked under its
//! body never changes `Transform` or `ChildOf` to trigger a rewrite.
//!
//! [`CellCoord`]: big_space::prelude::CellCoord

use avian3d::prelude::{ColliderOf, ColliderTransform, Rotation};
use bevy::prelude::*;

/// Matches a collider on the tick it gains its transform or its body, whichever lands last.
type NewlyAttached = Or<(Added<ColliderTransform>, Added<ColliderOf>)>;

pub(crate) fn collider_transform_from_hierarchy(
    mut colliders: Query<(Entity, &mut ColliderTransform, &ColliderOf), NewlyAttached>,
    hierarchy: Query<(&Transform, Option<&ChildOf>)>,
) {
    for (collider, mut collider_transform, collider_of) in &mut colliders {
        let Some(computed) = body_relative(collider, collider_of.body, &hierarchy) else {
            continue;
        };
        collider_transform.set_if_neq(computed);
    }
}

/// avian's propagation from `body` down to `collider`, or `None` if `collider` is not below
/// `body` through entities with a `Transform`.
fn body_relative(
    collider: Entity,
    body: Entity,
    hierarchy: &Query<(&Transform, Option<&ChildOf>)>,
) -> Option<ColliderTransform> {
    let mut below_body = Vec::new();
    let mut current = collider;
    while current != body {
        let (transform, child_of) = hierarchy.get(current).ok()?;
        below_body.push(*transform);
        current = child_of?.parent();
    }

    let mut scale = Vec3::ONE;
    let mut ancestor = Some(body);
    while let Some(entity) = ancestor {
        let (transform, child_of) = hierarchy.get(entity).ok()?;
        scale = transform.scale * scale;
        ancestor = child_of.map(ChildOf::parent);
    }

    let mut accumulated = ColliderTransform { scale, ..default() };
    for transform in below_body.into_iter().rev() {
        let child = ColliderTransform::from(transform);
        accumulated = ColliderTransform {
            translation: accumulated.transform_point(child.translation),
            rotation: Rotation(accumulated.rotation.0 * child.rotation.0),
            scale: accumulated.scale * child.scale,
        };
    }
    Some(accumulated)
}
